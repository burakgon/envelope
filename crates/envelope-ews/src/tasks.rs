//! Tasks: the Tasks folder and the task folders below it, task sync
//! (`SyncFolderItems` for the ids, `GetItem` for the fields), and creating,
//! changing and deleting tasks.
//!
//! Exchange keeps the start, due and completion dates of a task as days:
//! midnight in the time zone the task was set in, which it reads and
//! writes in the request's `TimeZoneContext` (UTC without one). The task
//! operations therefore take the user's time zone: they send it as the
//! request's `TimeZoneContext`, write days as midnight there, and read the
//! day a returned date starts there.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::client::{EwsClient, SoapRequest};
use crate::error::{EwsError, ResponseError};
use crate::folders::{DeleteType, Folder, FolderRole, FolderTraversal, order_tree};
use crate::items::{
    FieldUpdate, GET_ITEM_BATCH_SIZE, ItemChange, MAX_SYNC_CHANGES, SyncOptions, UpdateAttributes,
    first_item_id, parse_sync_folder_items, sync_folder_items_request, update_item_request,
    value_xml,
};
use crate::soap;
use crate::timezone::windows_zone_id;
use crate::types::{
    BaseShape, Body, BodyType, DistinguishedFolder, FolderId, FolderRef, Importance, ItemId,
    ItemKind, ItemRef, ItemShape, PropertyPath, child_datetime, format_datetime, string_list,
    wire_enum,
};
use crate::version::ExchangeVersion;
use crate::xml::{Element, XmlWriter};

wire_enum! {
    /// Progress of a task.
    pub enum TaskStatus {
        /// Not started.
        NotStarted = "NotStarted",
        /// In progress.
        InProgress = "InProgress",
        /// Completed.
        Completed = "Completed",
        /// Waiting on someone else.
        WaitingOnOthers = "WaitingOnOthers",
        /// Deferred.
        Deferred = "Deferred",
    }
}

/// A task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// Item id and change key.
    pub id: ItemId,
    /// The folder holding the task.
    pub parent_folder_id: Option<FolderId>,
    /// `IPM.Task`, or a custom task class.
    pub item_class: Option<String>,
    /// Title.
    pub subject: Option<String>,
    /// Notes, as plain text with `\n` line breaks.
    pub body: Option<String>,
    /// Importance.
    pub importance: Option<Importance>,
    /// Categories.
    pub categories: Vec<String>,
    /// The day work on the task is to start.
    pub start_date: Option<NaiveDate>,
    /// The day the task is due.
    pub due_date: Option<NaiveDate>,
    /// When the task was completed, as Exchange reports it (usually the
    /// start of the day; see [`Task::completed_at`]).
    pub complete_date: Option<DateTime<Utc>>,
    /// The day the task was completed.
    pub completed_on: Option<NaiveDate>,
    /// Progress.
    pub status: Option<TaskStatus>,
    /// How much is done, 0 to 100.
    pub percent_complete: Option<u8>,
    /// Whether the task is complete.
    pub is_complete: Option<bool>,
    /// Whether the task repeats.
    pub is_recurring: Option<bool>,
    /// Whether a reminder is set.
    pub reminder_is_set: Option<bool>,
    /// When the reminder goes off.
    pub reminder_due_by: Option<DateTime<Utc>>,
    /// When the task was created.
    pub created: Option<DateTime<Utc>>,
    /// When the task last changed (Exchange 2010 and later).
    pub last_modified: Option<DateTime<Utc>>,
}

impl Task {
    /// Parses a `Task` element; dates are read as days in `zone`.
    pub(crate) fn from_element(element: &Element, zone: Tz) -> Option<Task> {
        if !element.is("Task") {
            return None;
        }
        let day = |name: &str| child_datetime(element, name).and_then(|at| task_day(at, zone));
        let complete_date = child_datetime(element, "CompleteDate");
        Some(Task {
            id: element.child("ItemId").and_then(ItemId::from_element)?,
            parent_folder_id: element
                .child("ParentFolderId")
                .and_then(FolderId::from_element),
            item_class: element.child_text("ItemClass"),
            subject: element.child_raw_text("Subject"),
            body: element
                .child("Body")
                .map(|body| Body::from_element(body).content.replace("\r\n", "\n"))
                .filter(|text| !text.trim().is_empty()),
            importance: element
                .child_text("Importance")
                .and_then(|i| Importance::parse(&i)),
            categories: string_list(element.child("Categories")),
            start_date: day("StartDate"),
            due_date: day("DueDate"),
            completed_on: complete_date.and_then(|at| task_day(at, zone)),
            complete_date,
            status: element
                .child_text("Status")
                .and_then(|s| TaskStatus::parse(&s)),
            percent_complete: element
                .child_text("PercentComplete")
                .and_then(|p| p.trim().parse::<f64>().ok())
                .map(|p| p.round().clamp(0.0, 100.0) as u8),
            is_complete: element.child_bool("IsComplete"),
            is_recurring: element.child_bool("IsRecurring"),
            reminder_is_set: element.child_bool("ReminderIsSet"),
            reminder_due_by: child_datetime(element, "ReminderDueBy"),
            created: child_datetime(element, "DateTimeCreated"),
            last_modified: child_datetime(element, "LastModifiedTime"),
        })
    }

    /// True when the task is done.
    pub fn is_completed(&self) -> bool {
        self.is_complete == Some(true) || self.status == Some(TaskStatus::Completed)
    }

    /// When the task was completed, as well as Exchange tells: the last
    /// change when it came within a day after the reported completion
    /// (completing was most likely that change), else the reported
    /// completion. `None` for open tasks.
    pub fn completed_at(&self) -> Option<DateTime<Utc>> {
        if !self.is_completed() {
            return None;
        }
        match (self.complete_date, self.last_modified) {
            (Some(day), Some(modified))
                if modified >= day && modified - day < Duration::days(1) =>
            {
                Some(modified)
            }
            (Some(day), _) => Some(day),
            (None, modified) => modified,
        }
    }
}

/// The calendar day of a task date. A date at midnight UTC (written without
/// a time zone) is that day; any other is the midnight it is nearest to in
/// `zone`, where the task was set.
fn task_day(at: DateTime<Utc>, zone: Tz) -> Option<NaiveDate> {
    if at.time() == NaiveTime::MIN {
        return Some(at.date_naive());
    }
    let local = at.with_timezone(&zone).naive_local();
    let noon = NaiveTime::from_hms_opt(12, 0, 0)?;
    if local.time() >= noon {
        local.date().succ_opt()
    } else {
        Some(local.date())
    }
}

/// A task date as written: midnight, in the request's time zone.
fn day_value(day: NaiveDate) -> String {
    format!("{}T00:00:00", day.format("%Y-%m-%d"))
}

/// The properties fetched by [`EwsClient::get_tasks`].
pub fn task_properties(version: ExchangeVersion) -> Vec<PropertyPath> {
    let mut properties: Vec<PropertyPath> = [
        "item:ItemClass",
        "item:ParentFolderId",
        "item:Subject",
        "item:Body",
        "item:Importance",
        "item:Categories",
        "item:DateTimeCreated",
        "item:ReminderIsSet",
        "item:ReminderDueBy",
        "task:StartDate",
        "task:DueDate",
        "task:CompleteDate",
        "task:Status",
        "task:PercentComplete",
        "task:IsComplete",
        "task:IsRecurring",
    ]
    .into_iter()
    .map(PropertyPath::field)
    .collect();
    if version.has_2010_features() {
        properties.push(PropertyPath::field("item:LastModifiedTime"));
    }
    properties
}

/// One round of task synchronization: what changed in a task folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSyncPage {
    /// The state to store and send with the next call.
    pub sync_state: String,
    /// False when more changes are pending: call again with `sync_state`.
    pub includes_last_item_in_range: bool,
    /// Tasks that appeared in the folder (created, or moved in).
    pub created: Vec<ItemId>,
    /// Tasks that changed.
    pub updated: Vec<ItemId>,
    /// Items that left the folder (deleted, or moved out).
    pub deleted: Vec<ItemId>,
}

impl TaskSyncPage {
    /// The tasks to fetch with [`EwsClient::get_tasks`]: created, then
    /// updated ones.
    pub fn changed(&self) -> impl Iterator<Item = &ItemId> {
        self.created.iter().chain(&self.updated)
    }
}

/// A task to create.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTask {
    /// Title.
    pub subject: String,
    /// Notes, as plain text.
    pub body: Option<String>,
    /// Importance (`High` marks the task important).
    pub importance: Option<Importance>,
    /// The day work is to start. Exchange keeps a start date only on tasks
    /// with a due date, and no later than it.
    pub start_date: Option<NaiveDate>,
    /// The day the task is due.
    pub due_date: Option<NaiveDate>,
    /// Create the task already completed, at this instant.
    pub completed: Option<DateTime<Utc>>,
}

/// Changes to a task; fields left `None` stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskUpdate {
    /// New title.
    pub subject: Option<String>,
    /// New notes, as plain text (empty clears them).
    pub body: Option<String>,
    /// New importance.
    pub importance: Option<Importance>,
    /// New start day; `Some(None)` removes it. It must not fall after the
    /// due date.
    pub start_date: Option<Option<NaiveDate>>,
    /// New due day; `Some(None)` removes the due date, and the start date
    /// with it (Exchange keeps a start date only on tasks with a due date).
    pub due_date: Option<Option<NaiveDate>>,
    /// `Some(Some(at))` completes the task at `at`; `Some(None)` reopens it
    /// as not started, which clears the completion date and sets the
    /// percentage done to 0.
    pub completed: Option<Option<DateTime<Utc>>>,
}

impl TaskUpdate {
    fn is_empty(&self) -> bool {
        *self == TaskUpdate::default()
    }

    /// The field updates, in the order Exchange should apply them: the due
    /// date before the start date, and the completion (which also sets the
    /// status and the percentage done) last.
    fn field_updates(&self) -> Vec<FieldUpdate> {
        let mut updates = Vec::new();
        if let Some(subject) = &self.subject {
            updates.push(set("item:Subject", value_xml("Task", "Subject", subject)));
        }
        if let Some(body) = &self.body {
            let mut w = XmlWriter::new();
            w.start("t:Task", &[]);
            Body::text(body.as_str()).write(&mut w);
            w.end("t:Task");
            updates.push(set("item:Body", w.into_string()));
        }
        if let Some(importance) = self.importance {
            updates.push(set(
                "item:Importance",
                value_xml("Task", "Importance", importance.as_str()),
            ));
        }
        match self.due_date {
            Some(Some(day)) => {
                updates.push(set(
                    "task:DueDate",
                    value_xml("Task", "DueDate", &day_value(day)),
                ));
            }
            Some(None) => {
                updates.push(delete("task:DueDate"));
                updates.push(delete("task:StartDate"));
            }
            None => {}
        }
        if self.due_date != Some(None) {
            match self.start_date {
                Some(Some(day)) => updates.push(set(
                    "task:StartDate",
                    value_xml("Task", "StartDate", &day_value(day)),
                )),
                Some(None) => updates.push(delete("task:StartDate")),
                None => {}
            }
        }
        match self.completed {
            Some(Some(at)) => updates.push(set(
                "task:CompleteDate",
                value_xml("Task", "CompleteDate", &format_datetime(at)),
            )),
            Some(None) => updates.push(set(
                "task:Status",
                value_xml("Task", "Status", TaskStatus::NotStarted.as_str()),
            )),
            None => {}
        }
        updates
    }
}

fn set(path: &str, value_xml: String) -> FieldUpdate {
    FieldUpdate::set(PropertyPath::field(path), value_xml)
}

fn delete(path: &str) -> FieldUpdate {
    FieldUpdate::Delete(PropertyPath::field(path))
}

/// `CreateItem` of a task, its elements in schema order: the item's
/// (`Subject`, `Body`, `Importance`), then the task's (`CompleteDate`,
/// `DueDate`, `StartDate`).
pub(crate) fn create_task_request(folder: &FolderRef, task: &NewTask) -> String {
    let mut w = XmlWriter::new();
    w.start("m:CreateItem", &[])
        .start("m:SavedItemFolderId", &[]);
    folder.write(&mut w);
    w.end("m:SavedItemFolderId")
        .start("m:Items", &[])
        .start("t:Task", &[])
        .text_element("t:Subject", &task.subject);
    if let Some(body) = task.body.as_deref().filter(|b| !b.is_empty()) {
        Body::text(body).write(&mut w);
    }
    if let Some(importance) = task.importance {
        w.text_element("t:Importance", importance.as_str());
    }
    // Completing sets the status and the percentage done as well, so no
    // status is written for completed tasks; open ones start as not started.
    if let Some(at) = task.completed {
        w.text_element("t:CompleteDate", &format_datetime(at));
    }
    if let Some(day) = task.due_date {
        w.text_element("t:DueDate", &day_value(day));
    }
    if let Some(day) = task.start_date {
        w.text_element("t:StartDate", &day_value(day));
    }
    w.end("t:Task").end("m:Items").end("m:CreateItem");
    w.into_string()
}

impl EwsClient {
    /// The default Tasks folder (with [`FolderRole::Tasks`]) followed by the
    /// task folders below it, parents before children; hidden folders are
    /// left out.
    pub async fn task_folders(&self) -> Result<Vec<Folder>, EwsError> {
        let mut tasks = self.get_folder(DistinguishedFolder::Tasks).await?;
        tasks.role = Some(FolderRole::Tasks);
        let below: Vec<Folder> = self
            .find_folders(&tasks.id, FolderTraversal::Deep)
            .await?
            .into_iter()
            .filter(|folder| folder.is_tasks() && !folder.is_hidden)
            .collect();
        let root = tasks.id.id.clone();
        let mut folders = vec![tasks];
        folders.extend(order_tree(&root, below));
        Ok(folders)
    }

    /// One round of task synchronization for a task folder. Start without a
    /// state; repeat with the returned state while
    /// [`includes_last_item_in_range`](TaskSyncPage::includes_last_item_in_range)
    /// is false (the first rounds report every task as created). Fetch the
    /// changed tasks with [`EwsClient::get_tasks`]. An
    /// [`EwsError::requires_resync`] error means the state expired: start
    /// over without one and reconcile.
    pub async fn sync_tasks(
        &self,
        folder: impl Into<FolderRef>,
        sync_state: Option<&str>,
        max_changes: u32,
    ) -> Result<TaskSyncPage, EwsError> {
        let options = SyncOptions {
            max_changes: max_changes.clamp(1, MAX_SYNC_CHANGES),
            properties: vec![PropertyPath::field("item:ItemClass")],
            ignore: Vec::new(),
        };
        let body = sync_folder_items_request(&folder.into(), sync_state, &options);
        let response = self
            .call(&SoapRequest::read("SyncFolderItems", body))
            .await?;
        let page = parse_sync_folder_items(soap::single(&response)?)?;
        let mut sync = TaskSyncPage {
            sync_state: page.sync_state,
            includes_last_item_in_range: page.includes_last_item_in_range,
            created: Vec::new(),
            updated: Vec::new(),
            deleted: Vec::new(),
        };
        for change in page.changes {
            match change {
                ItemChange::Created(item) if item.kind == ItemKind::Task => {
                    sync.created.push(item.id);
                }
                ItemChange::Updated(item) if item.kind == ItemKind::Task => {
                    sync.updated.push(item.id);
                }
                ItemChange::Deleted { id } => sync.deleted.push(id),
                // Other items in a task folder (and read-state changes) are
                // not tasks.
                _ => {}
            }
        }
        Ok(sync)
    }

    /// Fetches tasks by id with [`task_properties`], the notes as plain
    /// text and the dates as days in `zone`, the user's time zone. One
    /// result per id, in order; a task that is gone reports
    /// `ErrorItemNotFound`.
    pub async fn get_tasks(
        &self,
        ids: &[ItemId],
        zone: Tz,
    ) -> Result<Vec<Result<Task, ResponseError>>, EwsError> {
        let shape = ItemShape {
            base: BaseShape::IdOnly,
            include_mime_content: false,
            body_type: Some(BodyType::Text),
            additional: task_properties(self.version()),
        };
        let refs: Vec<ItemRef> = ids.iter().map(ItemRef::from).collect();
        self.get_items_in_zone(
            &refs,
            &shape,
            GET_ITEM_BATCH_SIZE,
            Some(windows_zone_id(zone)),
            |element| Task::from_element(element, zone),
        )
        .await
    }

    /// One task (see [`EwsClient::get_tasks`]).
    pub async fn get_task(&self, id: &ItemId, zone: Tz) -> Result<Task, EwsError> {
        self.get_tasks(std::slice::from_ref(id), zone)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("GetItem returned nothing".into()))?
            .map_err(EwsError::from)
    }

    /// Creates a task in `folder` and returns its id. Days are written as
    /// midnight in `zone`, the user's time zone.
    pub async fn create_task(
        &self,
        folder: impl Into<FolderRef>,
        task: &NewTask,
        zone: Tz,
    ) -> Result<ItemId, EwsError> {
        let body = create_task_request(&folder.into(), task);
        let request = SoapRequest::write("CreateItem", body).in_time_zone(windows_zone_id(zone));
        let response = self.call(&request).await?;
        first_item_id(soap::single(&response)?)
            .ok_or_else(|| EwsError::InvalidResponse("CreateItem returned no item id".into()))
    }

    /// Changes a task and returns its id with the new change key. Days are
    /// written as midnight in `zone`, the user's time zone. Only the fields
    /// set in `update` are written, over the server's values even when the
    /// task changed there meanwhile; an empty update sends nothing.
    pub async fn update_task(
        &self,
        id: &ItemId,
        update: &TaskUpdate,
        zone: Tz,
    ) -> Result<ItemId, EwsError> {
        if update.is_empty() {
            return Ok(id.clone());
        }
        let body = update_item_request(
            &[(ItemRef::from(id), update.field_updates())],
            &UpdateAttributes::default(),
        );
        let request = SoapRequest::write("UpdateItem", body).in_time_zone(windows_zone_id(zone));
        let response = self.call(&request).await?;
        Ok(first_item_id(soap::single(&response)?).unwrap_or_else(|| id.clone()))
    }

    /// Deletes a task (every occurrence of a repeating one).
    pub async fn delete_task(&self, id: &ItemId, delete_type: DeleteType) -> Result<(), EwsError> {
        self.delete_items(std::slice::from_ref(id), delete_type)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EwsError::InvalidResponse("DeleteItem returned nothing".into()))?
            .map_err(EwsError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn task_days_are_midnight_where_they_were_set() {
        let istanbul = Tz::Europe__Istanbul;
        // Local midnight, as with a TimeZoneContext or from Outlook.
        assert_eq!(
            task_day(utc(2026, 11, 5, 21, 0), istanbul),
            Some(date(2026, 11, 6))
        );
        // Written without a zone: midnight UTC is the day itself.
        assert_eq!(
            task_day(utc(2026, 11, 6, 0, 0), Tz::Pacific__Auckland),
            Some(date(2026, 11, 6))
        );
        assert_eq!(
            task_day(utc(2026, 11, 6, 5, 0), Tz::America__New_York),
            Some(date(2026, 11, 6))
        );
        // Set in another zone: the nearest midnight.
        assert_eq!(
            task_day(utc(2026, 11, 5, 23, 0), istanbul),
            Some(date(2026, 11, 6))
        );
        assert_eq!(day_value(date(2026, 1, 9)), "2026-01-09T00:00:00");
    }

    #[test]
    fn parses_tasks() {
        let root = xml::parse(
            r#"<t:Items xmlns:t="t">
                 <t:Task>
                   <t:ItemId Id="AAMkTask1" ChangeKey="EwAAABYA1"/>
                   <t:ParentFolderId Id="tasks-id"/>
                   <t:ItemClass>IPM.Task</t:ItemClass>
                   <t:Subject>Send the invoice </t:Subject>
                   <t:Body BodyType="Text">Ask for the PO number&#13;
Then send it</t:Body>
                   <t:Importance>High</t:Importance>
                   <t:Categories><t:String>Work</t:String></t:Categories>
                   <t:DateTimeCreated>2026-10-20T08:00:00Z</t:DateTimeCreated>
                   <t:ReminderIsSet>false</t:ReminderIsSet>
                   <t:CompleteDate>2026-10-27T21:00:00Z</t:CompleteDate>
                   <t:DueDate>2026-10-29T21:00:00Z</t:DueDate>
                   <t:IsComplete>true</t:IsComplete>
                   <t:IsRecurring>false</t:IsRecurring>
                   <t:PercentComplete>100</t:PercentComplete>
                   <t:StartDate>2026-10-26T21:00:00Z</t:StartDate>
                   <t:Status>Completed</t:Status>
                   <t:LastModifiedTime>2026-10-28T10:42:00Z</t:LastModifiedTime>
                 </t:Task>
                 <t:Task><t:ItemId Id="AAMkTask2"/><t:Subject>Bare</t:Subject><t:Body BodyType="Text"></t:Body><t:PercentComplete>33.4</t:PercentComplete></t:Task>
                 <t:Message><t:ItemId Id="m1"/></t:Message>
               </t:Items>"#,
        )
        .unwrap();
        let zone = Tz::Europe__Istanbul;
        let tasks: Vec<Task> = root
            .elements()
            .filter_map(|el| Task::from_element(el, zone))
            .collect();
        assert_eq!(tasks.len(), 2);
        let invoice = &tasks[0];
        assert_eq!(
            invoice.id,
            ItemId::with_change_key("AAMkTask1", "EwAAABYA1")
        );
        assert_eq!(invoice.subject.as_deref(), Some("Send the invoice "));
        assert_eq!(
            invoice.body.as_deref(),
            Some("Ask for the PO number\nThen send it")
        );
        assert_eq!(invoice.importance, Some(Importance::High));
        assert_eq!(invoice.categories, ["Work"]);
        assert_eq!(invoice.due_date, Some(date(2026, 10, 30)));
        assert_eq!(invoice.start_date, Some(date(2026, 10, 27)));
        assert_eq!(invoice.completed_on, Some(date(2026, 10, 28)));
        assert_eq!(invoice.status, Some(TaskStatus::Completed));
        assert_eq!(invoice.percent_complete, Some(100));
        assert!(invoice.is_completed());
        assert_eq!(invoice.is_recurring, Some(false));
        assert_eq!(
            invoice.completed_at(),
            Some(utc(2026, 10, 28, 10, 42)),
            "the last change, on the day it was completed"
        );
        let bare = &tasks[1];
        assert_eq!(bare.body, None);
        assert_eq!(bare.percent_complete, Some(33));
        assert_eq!(bare.due_date, None);
        assert!(!bare.is_completed());
        assert_eq!(bare.completed_at(), None);
    }

    #[test]
    fn creates_tasks_in_schema_order() {
        let task = NewTask {
            subject: "Book the venue".into(),
            body: Some("Ask about <parking>".into()),
            importance: Some(Importance::High),
            start_date: Some(date(2026, 11, 2)),
            due_date: Some(date(2026, 11, 6)),
            completed: None,
        };
        assert_eq!(
            create_task_request(&DistinguishedFolder::Tasks.into(), &task),
            "<m:CreateItem><m:SavedItemFolderId><t:DistinguishedFolderId Id=\"tasks\"/></m:SavedItemFolderId>\
             <m:Items><t:Task><t:Subject>Book the venue</t:Subject>\
             <t:Body BodyType=\"Text\">Ask about &lt;parking&gt;</t:Body><t:Importance>High</t:Importance>\
             <t:DueDate>2026-11-06T00:00:00</t:DueDate><t:StartDate>2026-11-02T00:00:00</t:StartDate>\
             </t:Task></m:Items></m:CreateItem>"
        );
        let done = NewTask {
            subject: "Paid".into(),
            completed: Some(utc(2026, 10, 29, 8, 0)),
            due_date: Some(date(2026, 10, 30)),
            ..NewTask::default()
        };
        let body = create_task_request(&FolderId::new("F").into(), &done);
        assert!(body.contains(
            "<t:Subject>Paid</t:Subject><t:CompleteDate>2026-10-29T08:00:00Z</t:CompleteDate><t:DueDate>2026-10-30T00:00:00</t:DueDate></t:Task>"
        ));
        assert!(!body.contains("<t:Status>"));
    }

    #[test]
    fn updates_set_and_delete_fields() {
        let update = TaskUpdate {
            subject: Some("Book the bigger venue".into()),
            body: Some(String::new()),
            importance: Some(Importance::Normal),
            due_date: Some(None),
            start_date: Some(Some(date(2026, 11, 1))),
            completed: Some(None),
        };
        let body = update_item_request(
            &[(
                ItemRef::from(&ItemId::with_change_key("AAMkTask1", "ck")),
                update.field_updates(),
            )],
            &UpdateAttributes::default(),
        );
        assert_eq!(
            body,
            concat!(
                r#"<m:UpdateItem ConflictResolution="AlwaysOverwrite"><m:ItemChanges><t:ItemChange><t:ItemId Id="AAMkTask1" ChangeKey="ck"/><t:Updates>"#,
                r#"<t:SetItemField><t:FieldURI FieldURI="item:Subject"/><t:Task><t:Subject>Book the bigger venue</t:Subject></t:Task></t:SetItemField>"#,
                r#"<t:SetItemField><t:FieldURI FieldURI="item:Body"/><t:Task><t:Body BodyType="Text"></t:Body></t:Task></t:SetItemField>"#,
                r#"<t:SetItemField><t:FieldURI FieldURI="item:Importance"/><t:Task><t:Importance>Normal</t:Importance></t:Task></t:SetItemField>"#,
                r#"<t:DeleteItemField><t:FieldURI FieldURI="task:DueDate"/></t:DeleteItemField>"#,
                r#"<t:DeleteItemField><t:FieldURI FieldURI="task:StartDate"/></t:DeleteItemField>"#,
                r#"<t:SetItemField><t:FieldURI FieldURI="task:Status"/><t:Task><t:Status>NotStarted</t:Status></t:Task></t:SetItemField>"#,
                r#"</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
            )
        );
        let complete = TaskUpdate {
            due_date: Some(Some(date(2026, 11, 6))),
            start_date: Some(None),
            completed: Some(Some(utc(2026, 10, 29, 8, 0))),
            ..TaskUpdate::default()
        };
        let updates = complete.field_updates();
        assert_eq!(
            updates,
            [
                set(
                    "task:DueDate",
                    value_xml("Task", "DueDate", "2026-11-06T00:00:00")
                ),
                delete("task:StartDate"),
                set(
                    "task:CompleteDate",
                    value_xml("Task", "CompleteDate", "2026-10-29T08:00:00Z")
                ),
            ]
        );
        assert!(TaskUpdate::default().is_empty());
        assert!(TaskUpdate::default().field_updates().is_empty());
    }

    #[test]
    fn property_set_follows_the_version() {
        let modern = task_properties(ExchangeVersion::Exchange2013Sp1);
        assert!(modern.contains(&PropertyPath::field("task:IsRecurring")));
        assert!(modern.contains(&PropertyPath::field("item:LastModifiedTime")));
        let old = task_properties(ExchangeVersion::Exchange2007Sp1);
        assert!(!old.contains(&PropertyPath::field("item:LastModifiedTime")));
    }
}
