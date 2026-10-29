// Smoke test of the interface: serves the demo build (npm run build:demo)
// and walks through the main screens in Chromium, failing on any page error.
// Usage: npm run build:demo && npm run e2e   (CHROMIUM=/path/to/chromium to use a system browser)
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { chromium } from 'playwright';

const root = new URL('../dist-demo/', import.meta.url).pathname;
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.json': 'application/json' };

const server = createServer(async (req, res) => {
  const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname)).replace(/^(\.\.[/\\])+/, '');
  try {
    const file = join(root, path.endsWith('/') ? `${path}index.html` : path);
    const body = await readFile(file);
    res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const url = `http://127.0.0.1:${server.address().port}/`;

const browser = await chromium.launch({ executablePath: process.env.CHROMIUM || undefined });
const problems = [];
/** A window of the demo, its problems reported; `clock` sets the time it starts at. */
async function open(clock) {
  const p = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  p.on('pageerror', (e) => problems.push(`page error: ${e.message}`));
  p.on('console', (m) => m.type() === 'error' && problems.push(`console error: ${m.text()}`));
  // The app asks in dialogs of its own; the browser's are bare windows in WebKitGTK.
  p.on('dialog', (d) => {
    problems.push(`browser dialog: ${d.message()}`);
    d.dismiss();
  });
  await p.addInitScript(() => sessionStorage.setItem('envelope.demo.banner', 'hidden'));
  if (clock) await p.clock.install({ time: clock });
  return p;
}
const page = await open();

let failed = 0;
async function step(name, run) {
  try {
    await run();
    console.log(`ok   ${name}`);
  } catch (e) {
    failed++;
    console.log(`FAIL ${name}\n     ${String(e.message ?? e).split('\n')[0]}`);
  }
}
const visible = (selector) => page.locator(selector).first().waitFor({ state: 'visible', timeout: 5000 });
/** Waits for a condition the page reaches on its own; `failure` says what is wrong instead. */
async function until(check, failure) {
  for (const end = Date.now() + 5000; Date.now() < end; await page.waitForTimeout(100)) if (await check()) return;
  throw new Error(typeof failure === 'function' ? await failure() : failure);
}
/** A value once it has stopped changing (the views are read again shortly after each change). */
async function steady(read) {
  let last = await read();
  for (let i = 0; i < 20; i++) {
    await page.waitForTimeout(250);
    const now = await read();
    if (now === last) return now;
    last = now;
  }
  return last;
}

await step('the inbox lists conversations', async () => {
  await page.goto(url);
  await visible('.row');
  const rows = await page.locator('.row').count();
  if (rows < 5) throw new Error(`only ${rows} rows`);
});

await step('a conversation opens', async () => {
  await page.locator('.row').first().click();
  await visible('.reader .subject-block h2');
});

await step('j and k move through the list', async () => {
  const before = await page.locator('.reader .subject-block h2').innerText();
  await page.locator('body').press('j');
  await page.waitForTimeout(300);
  const after = await page.locator('.reader .subject-block h2').innerText();
  if (before === after) throw new Error('the open conversation did not change');
});

await step('m mutes the open conversation', async () => {
  const subject = (await page.locator('.reader .subject-block h2').innerText()).trim();
  const rows = () => page.locator('.row', { hasText: subject }).count();
  const before = await rows();
  await page.locator('body').press('m');
  await page.locator('.toast', { hasText: 'Muted' }).first().waitFor({ timeout: 5000 });
  await page.waitForTimeout(300);
  if ((await rows()) >= before) throw new Error(`"${subject}" is still in the list`);
  await page.locator('body').press('z');
});

await step('c opens the composer', async () => {
  await page.locator('body').press('c');
  await visible('.composer');
  await page.keyboard.press('Escape');
});

await step('the command palette opens the calendar', async () => {
  await page.keyboard.press('Control+k');
  await visible('.palette input');
  await page.keyboard.type('calendar');
  await page.keyboard.press('Enter');
  await visible('.grid-wrap');
});

await step('a new event opens the editor', async () => {
  await page.locator('button.compose').first().click();
  await visible('.editor[role="dialog"]');
  await page.keyboard.press('Escape');
});

await step('a repeating event reads its repeat rule in words, in the popover and the editor', async () => {
  await page.locator('.ev', { hasText: 'Gym' }).first().click();
  const line = page.locator('.pop .line', { hasText: /Weekly/ }).first();
  await until(async () => (await line.count()) > 0 && (await line.innerText()).trim() === 'Weekly on Monday, Wednesday, Friday', async () => `the popover says "${(await line.count()) ? await line.innerText() : 'nothing'}"`);
  await page.locator('.pop button[title="Edit"]').click();
  await visible('.editor[role="dialog"]');
  const repeat = page.locator('.editor select:has(option[value="FREQ=DAILY"])');
  const chosen = await repeat.evaluate((s) => s.options[s.selectedIndex].text);
  if (chosen !== 'Weekly on Monday, Wednesday, Friday') throw new Error(`the editor says "${chosen}"`);
  await page.keyboard.press('Escape');
  await page.locator('.editor[role="dialog"]').waitFor({ state: 'detached', timeout: 5000 });
});

await step('an event popover stays in the window as its details load', async () => {
  // A meeting with guests, scrolled to the bottom of the window: the popover grows as its
  // guests load and must move up rather than run off the bottom.
  const sync = page.locator('.ev', { hasText: 'Product sync' }).first();
  await sync.evaluate((el) => el.scrollIntoView({ block: 'end' }));
  await sync.click();
  await visible('.popover .pop');
  await until(async () => (await page.locator('.popover .pop .guest, .popover .pop [class*="guest"]').count()) > 0 || (await page.locator('.popover .pop').innerText()).includes('Daniel'), 'the guests did not load');
  const box = await steady(() => page.locator('.popover:has(.pop)').evaluate((el) => JSON.stringify(el.getBoundingClientRect())));
  const r = JSON.parse(box);
  const height = await page.evaluate(() => innerHeight);
  if (r.top < 7.5 || r.bottom > height - 7.5) throw new Error(`the popover is at ${Math.round(r.top)}–${Math.round(r.bottom)} in a window ${height} tall`);
  await page.keyboard.press('Escape');
  await page.locator('.popover .pop').waitFor({ state: 'detached', timeout: 5000 });
});

await step('closing an event or a contact editor with changes asks first', async () => {
  const ask = page.locator('[role="alertdialog"]');
  const editor = page.locator('.editor[role="dialog"]');
  // An event: Escape asks; going on keeps what was typed, and Discard closes.
  await page.locator('button.compose').first().click();
  await editor.waitFor({ timeout: 5000 });
  await page.keyboard.type('Planning lunch');
  await page.keyboard.press('Escape');
  await ask.waitFor({ timeout: 5000 });
  if (!(await ask.innerText()).includes('Discard your changes?')) throw new Error(`it asked: ${await ask.innerText()}`);
  await page.keyboard.press('Escape');
  await ask.waitFor({ state: 'detached', timeout: 5000 });
  if ((await editor.locator('input.title').inputValue()) !== 'Planning lunch') throw new Error('the title was lost');
  await page.keyboard.press('Escape');
  await ask.locator('button.danger').click();
  await editor.waitFor({ state: 'detached', timeout: 5000 });
  // A contact: Cancel asks too.
  await page.locator('.modes button[aria-label="Contacts"]').click();
  await page.locator('.contacts .row', { hasText: 'Maya Chen' }).click();
  const edit = page.locator('.detail button', { hasText: 'Edit' }).first();
  await edit.click();
  const first = page.locator('.contacts .editor input[placeholder="First name"]');
  await first.fill('Mia');
  await page.keyboard.press('Escape');
  await ask.waitFor({ timeout: 5000 });
  await ask.locator('button', { hasText: 'Keep editing' }).click();
  await ask.waitFor({ state: 'detached', timeout: 5000 });
  if ((await first.inputValue()) !== 'Mia') throw new Error('the change was lost');
  await page.locator('.contacts .editor button', { hasText: 'Cancel' }).click();
  await ask.locator('button.danger').click();
  await first.waitFor({ state: 'detached', timeout: 5000 });
  if (!(await page.locator('.detail').innerText()).includes('Maya Chen')) throw new Error('the discarded change was kept');
  // Unchanged, it closes at once.
  await edit.click();
  await first.waitFor({ timeout: 5000 });
  await first.focus();
  await page.keyboard.press('Escape');
  await first.waitFor({ state: 'detached', timeout: 5000 });
  if (await ask.count()) throw new Error('closing an unchanged contact asked');
  await page.locator('.modes button', { hasText: 'Calendar' }).click();
  await visible('.grid-wrap');
});

await step('settings open and close', async () => {
  await page.keyboard.press('Control+,');
  await visible('.window[role="dialog"]');
  await page.keyboard.press('Escape');
  await page.locator('.window[role="dialog"]').waitFor({ state: 'detached', timeout: 5000 });
});

await step('the interface switches language and back', async () => {
  await page.keyboard.press('Control+,');
  await visible('.window[role="dialog"]');
  // The first select on the General tab is the language; German loads as its own chunk.
  await page.locator('.window select').first().selectOption('de');
  await page.locator('.window .tab', { hasText: 'Allgemein' }).waitFor({ timeout: 5000 });
  await page.locator('.window select').first().selectOption('en');
  await page.locator('.window .tab', { hasText: 'General' }).waitFor({ timeout: 5000 });
  await page.keyboard.press('Escape');
});

await step('g k opens tasks', async () => {
  await page.locator('body').press('g');
  await page.locator('body').press('k');
  await visible('.tasks .quick input');
  const rows = await page.locator('.tasks .task').count();
  if (rows < 3) throw new Error(`only ${rows} tasks`);
});

await step('quick add understands a due date, a time and !', async () => {
  await page.locator('body').press('c');
  await page.locator('.quick input:focus').waitFor({ timeout: 5000 });
  await page.keyboard.type('Call Maya tomorrow 3pm !');
  const due = (await page.locator('.qchip.due').innerText()).trim();
  if (!/^Tomorrow, (3:00\sPM|15:00)$/.test(due)) throw new Error(`the due chip reads "${due}"`);
  await page.locator('.qchip.important').waitFor({ timeout: 5000 });
  await page.keyboard.press('Enter');
  await page.locator('.toast', { hasText: 'Added to Reminders' }).first().waitFor({ timeout: 5000 });
  if (await page.locator('.quick input').inputValue()) throw new Error('quick add was not cleared');
});

await step('quick add keeps what is typed while a task is saved', async () => {
  // The next task is typed right after Enter, before the first one comes back saved.
  const held = await page.evaluate(() => {
    const input = document.querySelector('.quick input');
    const type = (value) => {
      input.value = value;
      input.dispatchEvent(new Event('input', { bubbles: true }));
    };
    type('Buy milk');
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    type('Buy eggs');
    return new Promise((resolve) => setTimeout(() => resolve(input.value), 300));
  });
  if (held !== 'Buy eggs') throw new Error(`quick add holds "${held}"`);
  await page.locator('.tasks .task', { hasText: 'Buy milk' }).waitFor({ timeout: 5000 });
  await page.locator('.quick input').fill('');
});

await step('a task is completed', async () => {
  // Due tomorrow, so it is in Upcoming.
  await page.locator('.sidebar .item', { hasText: 'Upcoming' }).click();
  const row = page.locator('.tasks .task', { hasText: 'Call Maya' });
  await row.waitFor({ timeout: 5000 });
  if ((await row.locator('.title').innerText()).trim() !== 'Call Maya') throw new Error('the title kept the date');
  await row.locator('.check').click();
  await page.locator('.toast', { hasText: 'Task completed' }).first().waitFor({ timeout: 5000 });
  await row.waitFor({ state: 'detached', timeout: 5000 });
});

await step('t adds the open conversation to tasks', async () => {
  await page.locator('body').press('g');
  await page.locator('body').press('i');
  await page.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
  await visible('.reader .subject-block h2');
  await page.locator('body').press('t');
  const toast = page.locator('.toast', { hasText: 'Added to My Tasks' }).first();
  await toast.waitFor({ timeout: 5000 });
  await toast.locator('.action').click();
  await page.locator('.detail .source', { hasText: 'Q4 roadmap' }).waitFor({ timeout: 5000 });
});

await step('the keyboard acts on a subtask opened from the calendar', async () => {
  // A subtask with a date of its own; in its list only its task has a row.
  await page.locator('body').press('g');
  await page.locator('body').press('c');
  await page.locator('body').press('a');
  await page.locator('.tchip', { hasText: 'Pick up the frame' }).first().locator('.open').click();
  await page.locator('.detail .crumb', { hasText: 'Buy a birthday present for Mom' }).waitFor({ timeout: 5000 });
  await page.locator('body').press('x');
  await page.locator('.detail .head .check[aria-checked="true"]').waitFor({ timeout: 5000 });
  const parent = page.locator('.tasks .task', { hasText: 'Buy a birthday present for Mom' });
  await page.waitForTimeout(900);
  if (await parent.evaluate((el) => el.classList.contains('done'))) throw new Error('X completed the task, not its subtask');
});

await step('subtask checks are reached and ticked from the keyboard', async () => {
  await page.locator('.detail .crumb').click();
  await page.locator('.detail textarea.title').focus();
  for (let i = 0; i < 20; i++) {
    await page.keyboard.press('Tab');
    const label = await page.evaluate(() => document.activeElement?.getAttribute('aria-label') ?? '');
    if (label.includes('Wrap it')) break;
  }
  const check = page.locator('.detail .sub .check[aria-label*="Wrap it"]');
  if (!(await check.evaluate((el) => el === document.activeElement))) throw new Error('Tab never reached the subtask\'s check');
  await page.keyboard.press('Space');
  await page.locator('.detail .sub .check[aria-label*="Wrap it"][aria-checked="true"]').waitFor({ timeout: 5000 });
});

await step('a deleted task comes back with its subtasks', async () => {
  // Gone at once, from the list and its count; Undo brings it back with its subtasks.
  const row = page.locator('.tasks .task', { hasText: 'Buy a birthday present for Mom' });
  const count = () => page.locator('.sidebar .item', { hasText: 'Reminders' }).locator('.count').innerText();
  const subtasks = () => page.locator('.detail .sub:not(.add) .sub-title').evaluateAll((els) => els.map((e) => e.value).join(', '));
  const all = 'Order the photo book, Pick up the frame, Wrap it';
  await until(async () => (await subtasks()) === all, async () => `the task's subtasks are ${await subtasks()}`);
  const before = await steady(count);
  await page.locator('.detail .icon-btn[aria-label="Delete task"]').click();
  await row.waitFor({ state: 'detached', timeout: 5000 });
  await until(async () => (await count()) !== before, 'the list still counts the deleted task');
  await page.locator('.toast', { hasText: 'Task deleted' }).first().locator('.action').click();
  // It comes back selected, ready for the keyboard.
  await until(async () => (await row.getAttribute('aria-selected')) === 'true', 'the restored task is not selected');
  await row.click();
  await until(async () => (await subtasks()) === all, async () => `back with the subtasks ${await subtasks()}`);
  await until(async () => (await count()) === before, async () => `the list counts ${await count()}, not ${before} again`);
});

await step('clearing completed tasks deletes as many as it says', async () => {
  await page.locator('.tasks .ctoggle').click();
  const done = page.locator('.tasks .task.done');
  await done.first().waitFor({ timeout: 5000 });
  const shown = await done.count();
  await page.locator('.tasks .completed .clear').click();
  const asked = await page.locator('.ask').innerText();
  if (asked !== `Delete ${shown} completed task${shown === 1 ? '' : 's'}?`) throw new Error(`with ${shown} completed tasks it asks "${asked}"`);
  await page.locator('.btn.danger', { hasText: 'Delete' }).click();
  await page.locator('.toast', { hasText: `Deleted ${shown} completed task` }).first().waitFor({ timeout: 5000 });
  await done.first().waitFor({ state: 'detached', timeout: 5000 });
  if (await page.locator('.tasks .ctoggle').count()) throw new Error('completed tasks are still listed');
  // The completed subtasks of a task still open stay.
  await page.locator('.tasks .task', { hasText: 'Buy a birthday present for Mom' }).click();
  await page.locator('.detail .sub:not(.add)').nth(2).waitFor({ timeout: 5000 });
});

await step('a confirmation and a password prompt ask in the app', async () => {
  await page.keyboard.press('Escape');
  await page.keyboard.press('Control+,');
  await visible('.window[role="dialog"]');
  await page.locator('.window .tab', { hasText: 'Accounts' }).click();
  // Removing an account asks first: Escape says no and leaves the settings open, the red button yes.
  const remove = page.locator('.acct .icon-btn[title="Remove account"]').first();
  const ask = page.locator('[role="alertdialog"]');
  await remove.click();
  await ask.waitFor({ timeout: 5000 });
  await page.keyboard.press('Escape');
  await ask.waitFor({ state: 'detached', timeout: 5000 });
  if (!(await page.locator('.window[role="dialog"]').count())) throw new Error('Escape closed the settings as well');
  await remove.click();
  await ask.locator('button.danger').click();
  await page.locator('.toast', { hasText: 'Removed' }).first().waitFor({ timeout: 5000 });
  // A new password goes into a masked field, and Enter answers.
  await page.locator('.acct button', { hasText: 'Sign in again' }).nth(1).click();
  await page.locator('[role="dialog"] input[type="password"]:focus').waitFor({ timeout: 5000 });
  await page.keyboard.type('hunter2');
  await page.keyboard.press('Enter');
  await page.locator('.toast', { hasText: 'Signed in again' }).first().waitFor({ timeout: 5000 });
  await page.keyboard.press('Escape');
  await page.locator('.window[role="dialog"]').waitFor({ state: 'detached', timeout: 5000 });
});

await step('Today moves on at midnight', async () => {
  const late = new Date();
  late.setHours(23, 58, 0, 0);
  const night = await open(late);
  try {
    await night.goto(url);
    await night.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
    await night.locator('body').press('g');
    await night.locator('body').press('k');
    await night.locator('.sidebar .item', { hasText: 'Today' }).click();
    // Due tomorrow, so in Today once the day has changed.
    const tomorrow = night.locator('.tasks .task', { hasText: 'Prepare for the Q4 roadmap review' });
    await night.locator('.tasks .task').first().waitFor({ timeout: 5000 });
    if (await tomorrow.count()) throw new Error('a task due tomorrow is in Today before midnight');
    await night.clock.fastForward('05:00');
    await tomorrow.waitFor({ timeout: 5000 });
  } finally {
    await night.close();
  }
});

// Reading and writing mail, in a window of its own.
const w = await open();
await w.goto(url);
await w.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
/** Waits for a condition in that window. */
async function wUntil(check, failure) {
  for (const end = Date.now() + 5000; Date.now() < end; await w.waitForTimeout(100)) if (await check()) return;
  throw new Error(typeof failure === 'function' ? await failure() : failure);
}
/** Finds a conversation by searching for it, and opens it. */
async function openConversation(subject) {
  await w.locator('.search input').fill(subject);
  await w.locator('.row', { hasText: subject }).first().click();
  await w.locator('.reader .subject-block h2', { hasText: subject }).waitFor({ timeout: 5000 });
}
const editor = () => w.locator('.dock .composer .ProseMirror');
/** Closes the floating composer (which keeps the draft) and clears the search. */
async function closeComposer() {
  await w.locator('.dock .composer .bar .icon-btn').last().click();
  await w.locator('.dock .composer').waitFor({ state: 'detached', timeout: 5000 });
}

await step('the agenda puts a task due at a time among the events, in time order', async () => {
  // A Thursday morning: the standup (9:30) and the product sync (10:00) come before
  // the call due at 18:30.
  const thursday = new Date(2026, 10, 5, 8, 0);
  const day = await open(thursday);
  try {
    await day.goto(url);
    await day.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
    await day.keyboard.press('Control+k');
    await day.locator('.palette input').waitFor({ timeout: 5000 });
    await day.keyboard.type('calendar');
    await day.keyboard.press('Enter');
    await day.locator('.grid-wrap').waitFor({ timeout: 5000 });
    await day.getByRole('tab', { name: 'Agenda', exact: true }).click();
    const today = day.locator('.agenda section', { has: day.locator('.day.today') });
    await today.locator('.items').waitFor({ timeout: 5000 });
    const text = await today.locator('.items').innerText();
    const at = (title) => text.indexOf(title);
    const call = at('Call Mom about her birthday dinner');
    if (call < 0 || at('Daily standup') < 0 || at('Product sync') < 0) throw new Error(`today lists: ${text.replace(/\n+/g, ' | ')}`);
    if (call < at('Daily standup') || call < at('Product sync')) throw new Error(`out of time order: ${text.replace(/\n+/g, ' | ')}`);
  } finally {
    await day.close();
  }
});

await step('a short plain-text reply shows its text, its quote folded away', async () => {
  await openConversation('Book recommendations');
  const frame = w.frameLocator('.reader iframe').first();
  await frame.locator('.env-plain', { hasText: 'Pragmatic Programmer' }).first().waitFor({ state: 'visible', timeout: 5000 });
  if (await frame.locator('.env-attribution', { hasText: 'wrote:' }).isVisible()) throw new Error('the line before the quote is not folded away with it');
});

await step('the message frame fits its text, growing and shrinking with the quote', async () => {
  const frame = w.frameLocator('.reader iframe').first();
  const height = () => w.locator('.reader iframe').first().evaluate((el) => el.getBoundingClientRect().height);
  const short = await height();
  if (short > 80) throw new Error(`the frame of a two-line message is ${short}px tall`);
  await frame.locator('.env-quote-toggle').click();
  await wUntil(async () => (await height()) > short + 20, 'the frame did not grow with the quote');
  await frame.locator('.env-quote-toggle').click();
  await wUntil(async () => Math.abs((await height()) - short) <= 2, async () => `the frame stayed ${await height()}px tall, not ${short}px`);
});

await step('links say where they go, and warn when their text names another site', async () => {
  await openConversation('confirm your account');
  await w.frameLocator('.reader iframe').first().locator('a').first().hover();
  const pill = w.locator('.reader .pill.warn');
  await pill.waitFor({ timeout: 5000 });
  const said = await pill.innerText();
  if (!said.includes('northstar-verify.test') || !said.includes('northstar.test as the link text says')) throw new Error(`the warning says "${said}"`);
  await w.locator('.reader > header').hover();
  await pill.waitFor({ state: 'detached', timeout: 5000 });
});

await step('a draft in Drafts opens in the composer and comes back with its changes', async () => {
  await w.locator('.search .clear').click();
  await w.locator('body').press('g');
  await w.locator('body').press('d');
  await w.locator('.row', { hasText: 'Talk outline' }).first().click();
  await w.locator('.dock .composer .subject input').waitFor({ timeout: 5000 });
  if ((await w.locator('.dock .composer .subject input').inputValue()) !== 'Talk outline for the meetup') throw new Error('the composer has another subject');
  await w.locator('.dock .composer .att', { hasText: 'talk-outline.pdf' }).waitFor({ timeout: 5000 });
  await editor().locator('p', { hasText: 'Hi Maya' }).click();
  await w.keyboard.press('End');
  await w.keyboard.type(' a quick update:');
  await closeComposer();
  // Stored on the server again; opening it once more shows the change.
  const row = w.locator('.row', { hasText: 'Talk outline' }).first();
  await wUntil(async () => (await row.innerText()).includes('quick update'), 'Drafts does not show the saved draft');
  await row.click();
  await editor().locator('p', { hasText: 'Hi Maya, a quick update:' }).waitFor({ timeout: 5000 });
  await closeComposer();
});

await step('a reply draft opens under its conversation', async () => {
  await w.locator('.row', { hasText: 'Apartment viewing' }).first().click();
  const inline = w.locator('.reader .composer.inline');
  await inline.waitFor({ timeout: 5000 });
  if (!(await inline.locator('.ProseMirror').innerText()).includes('bring a friend along')) throw new Error('the inline composer has another text');
  if (await w.locator('.reader .card.draft').count()) throw new Error('the draft is shown as well as edited');
});

await step('a draft only on this computer is listed in Drafts', async () => {
  await w.locator('body').press('Escape');
  await w.locator('.sidebar .compose').click();
  await w.keyboard.type('noah.w@fastmail.test');
  await w.keyboard.press('Enter');
  await w.locator('.dock .composer .subject input').fill('Dinner on Friday');
  await editor().click();
  await w.keyboard.type('Are we still on for Friday?');
  const row = w.locator('.row', { hasText: 'Dinner on Friday' });
  await row.waitFor({ timeout: 5000 });
  await row.locator('.here').waitFor({ timeout: 5000 });
  await closeComposer();
});

await step('the trash button discards a draft, and Undo brings it back', async () => {
  await w.locator('.sidebar .compose').click();
  await w.locator('.dock .composer .subject input').fill('Lunch plans');
  await editor().click();
  await w.keyboard.type('Thai or ramen?');
  await w.locator('.dock .composer .icon-btn[aria-label="Discard draft"]').click();
  await w.locator('.dock .composer').waitFor({ state: 'detached', timeout: 5000 });
  await w.locator('.toast', { hasText: 'Draft discarded' }).locator('.action').click();
  await w.locator('.dock .composer .subject input').waitFor({ timeout: 5000 });
  if ((await w.locator('.dock .composer .subject input').inputValue()) !== 'Lunch plans') throw new Error('the subject did not come back');
  if (!(await editor().innerText()).includes('Thai or ramen?')) throw new Error('the text did not come back');
  await w.locator('.dock .composer .icon-btn[aria-label="Discard draft"]').click();
});

await step('a new message: writing goes above the signature, and From swaps the signature', async () => {
  await w.locator('.sidebar .compose').click();
  // From the work address, which has a signature.
  const from = w.locator('.dock .composer .from select');
  await from.selectOption({ index: 0 });
  await editor().locator('.envelope-signature').waitFor({ timeout: 5000 });
  await editor().click();
  await w.keyboard.type('Hello there.');
  const first = await editor().locator('p').first().innerText();
  if (first !== 'Hello there.') throw new Error(`the first line reads "${first}"`);
  if (!(await editor().locator('.envelope-signature').innerText()).includes('Product Lead')) throw new Error('the signature changed');
  await from.selectOption({ index: 1 });
  await wUntil(async () => !(await editor().locator('.envelope-signature').count()), 'the work signature stayed with the personal address');
  await from.selectOption({ index: 0 });
  await editor().locator('.envelope-signature', { hasText: 'Product Lead' }).waitFor({ timeout: 5000 });
  if (!(await editor().innerText()).startsWith('Hello there.')) throw new Error('the text changed with the signature');
});

await step('formatting: size, color, alignment, a picture, and clearing it', async () => {
  await w.locator('.dock .composer .icon-btn[title="Formatting"]').click();
  await editor().locator('p').first().click();
  await w.keyboard.press('Home');
  await w.keyboard.press('Shift+End');
  const tool = (name) => w.locator(`.dock .composer .toolbar button[aria-label="${name}"]`);
  await tool('Text size').click();
  await w.locator('.popover .menu-item', { hasText: 'Large' }).click();
  await tool('Text color').click();
  await w.locator('.popover .swatch[aria-label="#d93025"]').click();
  await editor().locator('span[style*="font-size: 18px"][style*="color"]').waitFor({ timeout: 5000 });
  await tool('Alignment').click();
  await w.locator('.popover button[aria-label="Center"]').click();
  await editor().locator('p[style*="text-align: center"]').waitFor({ timeout: 5000 });
  await tool('Clear formatting').click();
  const line = editor().locator('p').first();
  await wUntil(async () => !(await line.locator('span[style]').count()) && !(await line.getAttribute('style')), 'the formatting stayed');
  const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==';
  await w.locator('.dock .composer input.picker').setInputFiles({ name: 'dot.png', mimeType: 'image/png', buffer: Buffer.from(png, 'base64') });
  await editor().locator('img[src^="data:image/png"]').waitFor({ timeout: 5000 });
});

await step('Send asks about an address that looks wrong and a missing attachment', async () => {
  await w.locator('.dock .composer .chips input').first().fill('bob@example');
  await w.keyboard.press('Enter');
  await w.locator('.dock .composer .subject input').fill('The report');
  await editor().locator('p').first().click();
  await w.keyboard.press('End');
  await w.keyboard.type(' The report is attached.');
  const ask = w.locator('[role="alertdialog"]');
  await w.locator('.dock .composer .send-main').click();
  await ask.locator('h3', { hasText: 'Check this address' }).waitFor({ timeout: 5000 });
  await ask.locator('button', { hasText: 'Edit' }).click();
  await ask.waitFor({ state: 'detached', timeout: 5000 });
  if (!(await w.locator('.dock .composer').count())) throw new Error('Edit sent the message');
  await w.locator('.dock .composer .send-main').click();
  await ask.locator('button', { hasText: 'Send anyway' }).click();
  await ask.locator('h3', { hasText: 'Did you mean to attach a file?' }).waitFor({ timeout: 5000 });
  if (!(await ask.innerText()).includes('“attached”')) throw new Error(`the reminder reads "${await ask.innerText()}"`);
  await ask.locator('button', { hasText: 'Attach' }).click();
  await ask.waitFor({ state: 'detached', timeout: 5000 });
  if (!(await w.locator('.dock .composer').count())) throw new Error('Attach sent the message');
  // Answered both ways, it goes; and comes back with Undo.
  await w.locator('.dock .composer .send-main').click();
  await ask.locator('button', { hasText: 'Send anyway' }).click();
  await ask.locator('h3', { hasText: 'Did you mean to attach a file?' }).waitFor({ timeout: 5000 });
  await ask.locator('button', { hasText: 'Send anyway' }).click();
  await w.locator('.toast', { hasText: 'Sending' }).locator('.action').click();
  await w.locator('.dock .composer .subject input').waitFor({ timeout: 5000 });
  await closeComposer();
});

await step('a message is forwarded as an attachment, and all attachments are saved', async () => {
  await openConversation('Photos from the weekend');
  await w.locator('.reader .card .icon-btn[aria-label="More"]').first().click();
  await w.locator('.popover .menu-item', { hasText: 'Forward as attachment' }).click();
  await w.locator('.dock .composer .att', { hasText: '.eml' }).waitFor({ timeout: 5000 });
  await w.locator('.dock .composer .icon-btn[aria-label="Discard draft"]').click();
  await w.locator('.reader .save-all').click();
  const saved = w.locator('.toast', { hasText: 'Saved 3 attachments' });
  await saved.waitFor({ timeout: 5000 });
  if ((await saved.locator('.action').innerText()).trim() !== 'Show in folder') throw new Error('no way to show the saved files');
});

await step('deleting forever in Trash asks first, with the count', async () => {
  await openConversation('Tokyo office visit');
  await w.locator('body').press('#');
  await w.locator('.toast', { hasText: 'Moved to Trash' }).first().waitFor({ timeout: 5000 });
  await w.locator('.search .clear').click();
  await w.locator('.sidebar .item', { hasText: 'Trash' }).first().click();
  const row = w.locator('.row', { hasText: 'Tokyo office visit' });
  await row.click();
  const ask = w.locator('[role="alertdialog"]');
  await w.locator('body').press('#');
  await ask.locator('h3', { hasText: 'Delete this conversation forever?' }).waitFor({ timeout: 5000 });
  await w.keyboard.press('Escape');
  await ask.waitFor({ state: 'detached', timeout: 5000 });
  if (!(await row.count())) throw new Error('Escape deleted it');
  await w.locator('body').press('#');
  await ask.locator('button.danger', { hasText: 'Delete forever' }).click();
  await row.waitFor({ state: 'detached', timeout: 5000 });
});

await step('reply and forward headers are in the interface language', async () => {
  await w.keyboard.press('Control+,');
  await w.locator('.window select').first().selectOption('de');
  await w.locator('.window .tab', { hasText: 'Allgemein' }).waitFor({ timeout: 5000 });
  await w.keyboard.press('Escape');
  await openConversation('Hafta sonu');
  await w.locator('body').press('r');
  const inline = w.locator('.reader .composer.inline');
  await inline.locator('.quote-toggle').click();
  await inline.locator('.quoted', { hasText: 'schrieb Elif' }).waitFor({ timeout: 5000 });
  await inline.locator('.icon-btn[aria-label="Entwurf verwerfen"]').click();
  await w.locator('body').press('f');
  const fwd = w.locator('.dock .composer');
  await fwd.locator('.quote-toggle').click();
  const quoted = await fwd.locator('.quoted').innerText();
  if (!quoted.includes('Weitergeleitete Nachricht') || !quoted.includes('Von:') || !quoted.includes('Betreff:')) throw new Error(`the forwarded header reads "${quoted.slice(0, 120)}"`);
  await fwd.locator('.icon-btn[aria-label="Entwurf verwerfen"]').click();
  await w.keyboard.press('Control+,');
  await w.locator('.window select').first().selectOption('en');
  await w.locator('.window .tab', { hasText: 'General' }).waitFor({ timeout: 5000 });
  await w.keyboard.press('Escape');
});

await step('an inline reply pops out into a window of its own', async () => {
  await openConversation('Hafta sonu');
  await w.locator('body').press('r');
  await w.locator('.reader .composer.inline .icon-btn[aria-label="Pop out"]').click();
  await w.locator('.dock .composer').waitFor({ timeout: 5000 });
  await w.locator('.dock .composer .icon-btn[aria-label="Discard draft"]').click();
});

await w.close();

/** A fresh window of the demo, with `session` put in its sessionStorage first. */
async function fresh(session = {}) {
  const p = await open();
  await p.addInitScript((entries) => entries.forEach(([k, v]) => sessionStorage.setItem(k, v)), Object.entries(session));
  await p.goto(url);
  await p.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
  return p;
}
/** Polls a condition on page `p`. */
async function eventually(p, check, failure) {
  for (const end = Date.now() + 5000; Date.now() < end; await p.waitForTimeout(100)) if (await check()) return;
  throw new Error(typeof failure === 'function' ? await failure() : failure);
}
const personal = (p) => p.locator('.sidebar .account').nth(1);
const toast = (p, text) => p.locator('.toast', { hasText: text }).first().waitFor({ timeout: 5000 });

await step('account setup forgets the provider it backed out of', async () => {
  const p = await fresh();
  try {
    await p.locator('.sidebar .item.add').click();
    await p.locator('.detect input').fill('someone@icloud.com');
    await p.keyboard.press('Enter');
    await p.locator('#pw-pass').fill('an-app-password');
    await p.locator('.back').click();
    // Google's sign-in is not offered the iCloud address.
    await p.locator('.prov', { hasText: 'Google' }).click();
    if ((await p.locator('#ob-email').inputValue()) !== '') throw new Error('Google sign-in got the iCloud address');
    await p.locator('.back').click();
    await p.locator('.prov', { hasText: 'Other' }).click();
    if (await p.locator('#pw-pass').inputValue()) throw new Error('the password stayed');
    await p.locator('.adv-toggle').click();
    const host = p.locator('.srv input').first();
    if (await host.inputValue()) throw new Error(`the iCloud server stayed: ${await host.inputValue()}`);
    // A new address finds its own servers; another domain finds them again.
    await p.locator('#pw-email').fill('me@posteo.de');
    await p.locator('#pw-pass').click();
    await eventually(p, async () => (await host.inputValue()) === 'imap.posteo.de', async () => `the server is ${await host.inputValue()}`);
    await p.locator('#pw-email').fill('me@icloud.com');
    await p.locator('#pw-pass').click();
    await eventually(p, async () => (await host.inputValue()) === 'imap.icloud.com', async () => `after another domain the server is ${await host.inputValue()}`);
    // A server typed by hand stays.
    await host.fill('mail.example.org');
    await p.locator('#pw-email').fill('me@example.org');
    await p.locator('#pw-pass').click();
    await p.waitForTimeout(600);
    if ((await host.inputValue()) !== 'mail.example.org') throw new Error('a server typed by hand was replaced');
  } finally {
    await p.close();
  }
});

await step('folders are made, renamed, nested, colored and deleted in the sidebar', async () => {
  const p = await fresh();
  try {
    const item = (name) => p.locator('.sidebar .account .item', { hasText: name });
    const prompt = p.locator('[role="dialog"] input');
    await item('Travel').click({ button: 'right' });
    await p.locator('.menu-item', { hasText: 'New subfolder' }).click();
    await prompt.waitFor({ timeout: 5000 });
    await p.keyboard.type('Paris');
    await p.keyboard.press('Enter');
    await toast(p, 'Folder “Paris” created');
    // Inside Travel: indented below it.
    const indent = async (name) => parseFloat(await item(name).evaluate((el) => getComputedStyle(el).paddingLeft));
    if ((await indent('Paris')) <= (await indent('Travel'))) throw new Error('Paris is not inside Travel');
    await item('Paris').click({ button: 'right' });
    await p.locator('.menu-item', { hasText: 'Rename' }).click();
    await prompt.fill('Lyon');
    await p.keyboard.press('Enter');
    await toast(p, 'Renamed to “Lyon”');
    if (await item('Paris').count()) throw new Error('Paris is still listed');
    // A name already used there is refused before anything is asked of the server.
    await personal(p).locator('.add-folder').click();
    await prompt.fill('Receipts');
    await p.keyboard.press('Enter');
    await p.locator('[role="dialog"] .message', { hasText: 'already a folder called “Receipts”' }).waitFor({ timeout: 5000 });
    await p.keyboard.press('Escape');
    // Dragged onto Travel, Receipts goes inside it; Undo brings it back.
    await item('Receipts').dragTo(item('Travel'));
    await toast(p, 'Moved “Receipts” into “Travel”');
    if ((await indent('Receipts')) <= (await indent('Travel'))) throw new Error('Receipts did not go inside Travel');
    await p.locator('.toast', { hasText: 'Moved “Receipts”' }).first().locator('.action').click();
    await eventually(p, async () => (await indent('Receipts')) === (await indent('Travel')), 'Undo left Receipts inside Travel');
    // A label gets a color.
    await item('Design').click({ button: 'right' });
    await p.locator('.swatches .sw[aria-label="Teal"]').click();
    await eventually(p, async () => (await item('Design').locator('.label-dot').evaluate((el) => getComputedStyle(el).backgroundColor)) === 'rgb(0, 183, 195)', 'Design is not teal');
    // Deleting asks first, with what happens to the conversations.
    await item('Lyon').click({ button: 'right' });
    await p.locator('.menu-item', { hasText: 'Delete folder' }).click();
    const ask = p.locator('[role="alertdialog"]');
    await ask.locator('h3', { hasText: 'Delete folder “Lyon”?' }).waitFor({ timeout: 5000 });
    await ask.locator('.message', { hasText: 'It is empty.' }).waitFor({ timeout: 5000 });
    await ask.locator('button.danger').click();
    await toast(p, 'Deleted “Lyon”');
    if (await item('Lyon').count()) throw new Error('Lyon is still listed');
  } finally {
    await p.close();
  }
});

await step('“Create ‘name’” in the Move and Label menus', async () => {
  const p = await fresh();
  try {
    await p.locator('.row', { hasText: 'Hafta sonu planı?' }).click();
    await p.locator('body').press('v');
    await p.keyboard.type('Clients');
    const create = p.locator('.menu-item.create');
    if ((await create.innerText()).trim() !== 'Create folder “Clients”') throw new Error(`the menu offers "${await create.innerText()}"`);
    await p.keyboard.press('Enter');
    await toast(p, 'Moved to Clients');
    await personal(p).locator('.item', { hasText: 'Clients' }).click();
    await p.locator('.row', { hasText: 'Hafta sonu planı?' }).waitFor({ timeout: 5000 });
    // Gmail: a new label, put on the conversation.
    await p.locator('body').press('g');
    await p.locator('body').press('i');
    await p.locator('.row', { hasText: 'New onboarding flow' }).click();
    await p.locator('body').press('l');
    await p.keyboard.type('Urgent');
    await p.locator('.menu-item.create', { hasText: 'Create label “Urgent”' }).waitFor({ timeout: 5000 });
    await p.keyboard.press('Enter');
    await toast(p, 'Added label “Urgent”');
    await p.locator('.row', { hasText: 'New onboarding flow' }).locator('.label', { hasText: 'Urgent' }).waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('Empty Trash asks with the count, then deletes everything', async () => {
  const p = await fresh();
  try {
    // Near the top of the list, clear of the toasts at the bottom.
    for (const subject of ['Hafta sonu planı?', 'Photos from the weekend']) {
      await p.locator('.row', { hasText: subject }).click();
      await p.locator('body').press('#');
      await toast(p, 'Moved to Trash');
    }
    const trash = personal(p).locator('.item', { hasText: 'Trash' });
    await trash.click();
    await eventually(p, async () => (await p.locator('.row').count()) === 2, 'Trash does not list the two conversations');
    await trash.click({ button: 'right' });
    await p.locator('.menu-item', { hasText: 'Empty Trash' }).click();
    // The question deleting forever asks, with the count.
    const ask = p.locator('[role="alertdialog"]');
    await ask.locator('h3', { hasText: 'Delete 2 conversations forever?' }).waitFor({ timeout: 5000 });
    await ask.locator('button.danger', { hasText: 'Delete forever' }).click();
    await toast(p, 'Deleted 2 conversations');
    await eventually(p, async () => (await p.locator('.row').count()) === 0, 'Trash is not empty');
  } finally {
    await p.close();
  }
});

await step('Not spam brings a conversation back, and ! toggles', async () => {
  const p = await fresh();
  try {
    const subject = 'You have been selected for a $500 gift card';
    await personal(p).locator('.item', { hasText: 'Spam' }).click();
    const row = p.locator('.row', { hasText: subject });
    await row.click({ button: 'right' });
    await p.locator('.menu-item', { hasText: 'Not spam' }).waitFor({ timeout: 5000 });
    if (await p.locator('.menu-item', { hasText: 'Report spam' }).count()) throw new Error('Spam still offers Report spam');
    await p.keyboard.press('Escape');
    await row.click();
    await p.locator('.reader .pill', { hasText: 'Not spam' }).waitFor({ timeout: 5000 });
    await p.locator('body').press('!');
    await toast(p, 'Not spam · moved to Inbox');
    await row.waitFor({ state: 'detached', timeout: 5000 });
    await personal(p).locator('.item', { hasText: 'Inbox' }).click();
    // Under the inbox's All tab (it is filed as a notification).
    await p.locator('body').press('4');
    await row.click();
    // In the inbox, ! reports it again.
    await p.locator('body').press('!');
    await toast(p, 'Marked as spam');
  } finally {
    await p.close();
  }
});

await step('Mark all as read takes only the tab shown, says how many and undoes', async () => {
  const p = await fresh();
  try {
    const badge = (tab) => p.locator('.tabs button', { hasText: tab }).locator('.badge');
    const people = Number(await badge('People').innerText());
    const newsletters = await badge('Newsletters').innerText();
    await p.locator('.list .tools .icon-btn[title="Filter"]').click();
    await p.locator('.menu-item', { hasText: 'Mark all as read' }).click();
    await toast(p, `Marked ${people} conversation${people === 1 ? '' : 's'} as read`);
    await badge('People').waitFor({ state: 'detached', timeout: 5000 });
    if ((await badge('Newsletters').innerText()) !== newsletters) throw new Error('another tab was marked read');
    await p.locator('.toast', { hasText: 'as read' }).first().locator('.action').click();
    await eventually(p, async () => (await badge('People').count()) && Number(await badge('People').innerText()) === people, 'Undo did not bring the unread back');
  } finally {
    await p.close();
  }
});

await step('a follow-up reminder is done, or comes back later', async () => {
  const p = await fresh();
  try {
    await p.locator('.sidebar .item', { hasText: 'Follow up' }).click();
    const row = p.locator('.row', { hasText: 'Contract renewal' });
    await row.click();
    await p.locator('.reader .pill', { hasText: 'Done' }).click();
    await toast(p, 'Follow-up done');
    await row.waitFor({ state: 'detached', timeout: 5000 });
    await p.locator('body').press('z');
    await row.click();
    await p.locator('.reader .icon-btn[aria-label="Remind me again…"]').click();
    await p.locator('.menu-item', { hasText: 'Tomorrow' }).click();
    await toast(p, 'Follow-up moved to');
    await row.waitFor({ state: 'detached', timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('blocking a sender moves their mail to Spam; Privacy unblocks', async () => {
  const p = await fresh();
  try {
    const row = p.locator('.row', { hasText: 'Apartment viewing on Saturday' });
    await row.click();
    // Her message is folded above the reply being written.
    await p.locator('.reader .collapsed', { hasText: 'Hannah Becker' }).click();
    const card = p.locator('.card').filter({ has: p.locator('.row1 .name', { hasText: 'Hannah Becker' }) });
    await card.locator('.icon-btn[title="More"]').click();
    await p.locator('.menu-item', { hasText: 'Block sender' }).click();
    const ask = p.locator('[role="alertdialog"]');
    await ask.locator('h3', { hasText: 'Block Hannah Becker?' }).waitFor({ timeout: 5000 });
    await ask.locator('.message', { hasText: 'already here moves there too' }).waitFor({ timeout: 5000 });
    await ask.locator('button.danger', { hasText: 'Block' }).click();
    await toast(p, 'Blocked hannah.becker@mail.test · 1 conversation moved to Spam');
    await row.waitFor({ state: 'detached', timeout: 5000 });
    await p.keyboard.press('Control+,');
    await p.locator('.window .tab', { hasText: 'Privacy' }).click();
    const blocked = p.locator('.blocked', { hasText: 'hannah.becker@mail.test' });
    await blocked.locator('button', { hasText: 'Unblock' }).click();
    await toast(p, 'Unblocked hannah.becker@mail.test');
    await p.locator('.blocked-list', { hasText: 'You haven’t blocked anyone.' }).waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('a rule runs on mail already here, with a count and Undo', async () => {
  const p = await fresh();
  try {
    await p.keyboard.press('Control+,');
    await p.locator('.window .tab', { hasText: 'Rules' }).click();
    await p.locator('.window .btn', { hasText: 'New rule' }).click();
    await p.locator('#rule-name').fill('Bank');
    // The bank's statements, not the look-alike address of the link warning.
    await p.locator('.editor-card .line input').first().fill('statements@northstar.test');
    await p.locator('.editor-card .stop', { hasText: 'Also apply to existing messages' }).locator('.toggle').click();
    await p.locator('.editor-card .btn.primary', { hasText: 'Save rule' }).click();
    await toast(p, '“Bank” applied to 1 conversation');
    await p.locator('.toast', { hasText: '“Bank” applied' }).first().locator('.action').click();
    await p.locator('.acct.rule', { hasText: 'Bank' }).locator('.btn', { hasText: 'Run now' }).click();
    await toast(p, '“Bank” applied to 1 conversation');
  } finally {
    await p.close();
  }
});

await step('a banner asks to sign in again, and says when a server is out of reach', async () => {
  const trouble = {
    1: { state: 'offline', message: 'connection refused (os error 111)', failures: 4 },
    2: { state: 'authError', message: 'Invalid credentials' },
  };
  const p = await fresh({ 'envelope.demo.sync': JSON.stringify(trouble) });
  try {
    const signIn = p.locator('.alert', { hasText: 'Personal needs you to sign in again.' });
    await signIn.locator('.btn', { hasText: 'Sign in' }).click();
    await p.locator('[role="dialog"] input[type="password"]:focus').waitFor({ timeout: 5000 });
    await p.keyboard.type('hunter2');
    await p.keyboard.press('Enter');
    await toast(p, 'Signed in again');
    await signIn.waitFor({ state: 'detached', timeout: 5000 });
    const unreachable = p.locator('.alert', { hasText: 'Work can’t reach imap.gmail.com.' });
    await unreachable.locator('.btn', { hasText: 'Details' }).click();
    await p.locator('[role="alertdialog"] .detail', { hasText: 'connection refused' }).waitFor({ timeout: 5000 });
    await p.locator('[role="alertdialog"] .btn.primary', { hasText: 'Retry' }).click();
    await toast(p, 'Trying Work again…');
    await p.keyboard.press('Control+,');
    await p.locator('.window .tab', { hasText: 'Accounts' }).click();
    await p.locator('.acct-problem', { hasText: 'Work can’t reach imap.gmail.com.' }).waitFor({ timeout: 5000 });
    await p.keyboard.press('Escape');
    await unreachable.locator('.close').click();
    await unreachable.waitFor({ state: 'detached', timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('accounts set up in another mail program are offered and fill in the form', async () => {
  const p = await open();
  try {
    await p.goto(url);
    await p.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
    await p.locator('.sidebar .item.add').click();
    const found = p.locator('.found-row', { hasText: 'alex.morgan@posteo.test' });
    await found.waitFor({ timeout: 5000 });
    if (!(await found.innerText()).includes('Set up in Thunderbird')) throw new Error(`the row says: ${await found.innerText()}`);
    await found.click();
    await p.locator('#pw-pass').waitFor({ timeout: 5000 });
    const value = (selector) => p.locator(selector).inputValue();
    if ((await value('#pw-email')) !== 'alex.morgan@posteo.test' || (await value('#pw-name')) !== 'Alex Morgan') throw new Error('the address or the name was not filled in');
    if (!(await p.locator('.card .sub', { hasText: 'Settings taken from Thunderbird' }).count())) throw new Error('the form does not say where its settings came from');
    await p.locator('.adv-toggle').click();
    if ((await p.locator('.srv input').first().inputValue()) !== 'imap.posteo.test') throw new Error('the incoming server was not filled in');
    // Back on the list and on to Other, the address stays but the other program's servers go.
    await p.locator('.card .back').click();
    await p.locator('.found-row').first().waitFor({ timeout: 5000 });
    await p.locator('.grid .prov').last().click();
    await p.locator('.adv-toggle').click();
    if ((await p.locator('.srv input').first().inputValue()) !== '' || (await p.locator('.card .sub', { hasText: 'Thunderbird' }).count())) throw new Error('the other program\'s settings stayed');
  } finally {
    await p.close();
  }
});

await step('contacts are imported from a CSV export, leaving out people already there', async () => {
  const p = await open();
  try {
    await p.goto(url);
    await p.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
    await p.locator('.modes button[aria-label="Contacts"]').click();
    await p.locator('.contacts .row').first().waitFor({ timeout: 5000 });
    await p.locator('.contacts header .icon-btn[aria-label="More"]').click();
    await p.locator('.menu-item', { hasText: 'Import contacts' }).click();
    const preview = p.locator('.backdrop .calendar');
    await preview.waitFor({ timeout: 5000 });
    // Maya Chen is in the address book already: listed, but not added again.
    const maya = preview.locator('li', { hasText: 'Maya Chen' });
    if (!(await maya.locator('input').isDisabled())) throw new Error('someone already there can be added again');
    await preview.locator('button.primary', { hasText: 'Add 2 contacts' }).click();
    await p.locator('.toast', { hasText: 'Added 2 contacts' }).first().waitFor({ timeout: 5000 });
    await p.locator('.contacts .row', { hasText: 'Jonas Lindqvist' }).waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('account settings are edited after setup, and a wrong server is caught before saving', async () => {
  const p = await open();
  try {
    await p.goto(url);
    await p.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
    await p.keyboard.press('Control+,');
    await p.locator('.window .tab', { hasText: 'Accounts' }).click();
    const row = p.locator('.acct', { hasText: 'alex.morgan@icloud.com' });
    const dialog = p.locator('[role="dialog"][aria-labelledby="acct-title"]');
    // Unchanged, it closes without asking.
    await row.locator('.icon-btn[aria-label="Account settings"]').click();
    await dialog.waitFor({ timeout: 5000 });
    await p.keyboard.press('Escape');
    await dialog.waitFor({ state: 'detached', timeout: 5000 });
    // A server that doesn't answer: nothing is saved until "Save anyway".
    await row.locator('.icon-btn[aria-label="Account settings"]').click();
    const host = dialog.locator('.srv input').first();
    const was = await host.inputValue();
    await host.fill('imap.wrong.example');
    await dialog.locator('button.primary', { hasText: 'Save' }).click();
    await dialog.locator('.failure', { hasText: 'Could not connect' }).waitFor({ timeout: 5000 });
    if (!(await dialog.locator('button', { hasText: 'Save anyway' }).count())) throw new Error('no way to save anyway');
    // The right server again: tested, then saved with the time kept on this computer.
    await host.fill(was);
    await dialog.locator('button', { hasText: 'Test connection' }).click();
    await dialog.locator('.passed').waitFor({ timeout: 5000 });
    await dialog.locator('#acct-keep').selectOption({ label: 'From the last year' });
    await dialog.locator('#acct-notify').selectOption({ label: 'Mail from people only' });
    await dialog.locator('button.primary', { hasText: 'Save' }).click();
    await p.locator('.toast', { hasText: 'Account settings saved' }).first().waitFor({ timeout: 5000 });
    await dialog.waitFor({ state: 'detached', timeout: 5000 });
    await row.locator('.icon-btn[aria-label="Account settings"]').click();
    if ((await dialog.locator('#acct-keep option:checked').innerText()) !== 'From the last year') throw new Error('the time kept was not saved');
    if ((await dialog.locator('#acct-notify option:checked').innerText()) !== 'Mail from people only') throw new Error('the notification choice was not saved');
  } finally {
    await p.close();
  }
});

await step('duplicate contacts are found and merged into the fuller one', async () => {
  const p = await open();
  try {
    await p.goto(url);
    await p.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
    await p.locator('.modes button[aria-label="Contacts"]').click();
    await p.locator('.contacts .row', { hasText: 'Dan Okafor' }).waitFor({ timeout: 5000 });
    await p.locator('.contacts header .icon-btn[aria-label="More"]').click();
    await p.locator('.menu-item', { hasText: 'Find duplicates' }).click();
    const dialog = p.locator('[aria-labelledby="dup-title"]');
    const group = dialog.locator('.group').first();
    await group.waitFor({ timeout: 5000 });
    // The fuller contact is the one kept.
    if (!(await group.locator('.person.kept').innerText()).includes('Daniel Okafor')) throw new Error(`keeps: ${await group.locator('.person.kept').innerText()}`);
    await group.locator('button', { hasText: 'Merge' }).click();
    await p.locator('.toast', { hasText: 'Merged 1 duplicate' }).first().waitFor({ timeout: 5000 });
    await dialog.locator('.none').waitFor({ timeout: 5000 });
    await dialog.locator('button', { hasText: 'Close' }).click();
    await p.locator('.contacts .row', { hasText: 'Dan Okafor' }).waitFor({ state: 'detached', timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('no page errors', async () => {
  if (problems.length) throw new Error(problems.join('; '));
});

await browser.close();
server.close();
if (failed) {
  console.log(`\n${failed} step(s) failed`);
  process.exit(1);
}
