-- Lists in other orders than newest first, and lists of messages one by one
-- (schema version 13). A sender sorts by sort_key(name, email), a function
-- every connection has (db::mail::sort_key).
CREATE INDEX IF NOT EXISTS thread_folders_sender ON thread_folders(folder_id, sender, last_date DESC);
CREATE INDEX IF NOT EXISTS message_folders_date ON message_folders(folder_id, date DESC);
CREATE INDEX IF NOT EXISTS message_folders_sender ON message_folders(folder_id, sender, date DESC);

-- A message's date and sender in its folders follow it when they change (a
-- draft edited elsewhere). Putting a message in a folder gives them
-- (db::mail::add_location): a trigger on every insert would slow sync down.
CREATE TRIGGER IF NOT EXISTS messages_sort AFTER UPDATE OF date, from_name, from_email ON messages
BEGIN
    UPDATE message_folders SET date = NEW.date, sender = sort_key(NEW.from_name, NEW.from_email)
    WHERE message_id = NEW.id;
END;
