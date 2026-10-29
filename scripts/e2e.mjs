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
/** A window of the demo, its problems reported; `clock` sets the time it starts at, `timezoneId` the computer's zone. */
async function open(clock, { timezoneId } = {}) {
  const p = await browser.newPage({ viewport: { width: 1440, height: 900 }, timezoneId });
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

await step('birthdays from the contacts appear in the calendar, and week numbers on request', async () => {
  const day = await open(new Date(2026, 10, 2, 9, 0));
  try {
    await day.goto(url);
    await day.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
    await day.keyboard.press('Control+k');
    await day.locator('.palette input').waitFor({ timeout: 5000 });
    await day.keyboard.type('calendar');
    await day.keyboard.press('Enter');
    await day.locator('.grid-wrap').waitFor({ timeout: 5000 });
    await day.getByRole('tab', { name: 'Month', exact: true }).click();
    // Linda Morgan (born 1961) has her birthday on November 6.
    const linda = day.locator('.month .item', { hasText: 'Linda Morgan’s birthday' });
    await linda.waitFor({ timeout: 5000 });
    await linda.click();
    const pop = day.locator('.popover', { hasText: 'Turns 65' });
    await pop.waitFor({ timeout: 5000 });
    await pop.locator('.menu-item', { hasText: 'Open contact' }).click();
    await day.locator('.contacts h2', { hasText: 'Linda Morgan' }).waitFor({ timeout: 5000 });
    // The Birthdays calendar can be hidden.
    await day.locator('.sidebar .seg [role="tab"], .sidebar [role="tab"]', { hasText: 'Calendar' }).first().click();
    await linda.waitFor({ timeout: 5000 });
    const toggle = day.getByRole('checkbox', { name: 'Birthdays' });
    await toggle.click();
    await linda.waitFor({ state: 'detached', timeout: 5000 });
    await toggle.click();
    await linda.waitFor({ timeout: 5000 });
    // Week numbers: in the corner of each week's first day, and above the week view.
    if (await day.locator('.month .wk').count()) throw new Error('week numbers show before they are turned on');
    await day.keyboard.press('Control+,');
    await day.locator('.window .tab', { hasText: 'Calendar' }).click();
    await day.getByRole('switch', { name: 'Week numbers' }).click();
    await day.keyboard.press('Escape');
    await eventually(day, async () => (await day.locator('.month .wk').allInnerTexts()).join(' ') === '44 45 46 47 48 49', async () => `the weeks read ${(await day.locator('.month .wk').allInnerTexts()).join(' ')}`);
    await day.getByRole('tab', { name: 'Week', exact: true }).click();
    await day.locator('.calendar header .week-no', { hasText: 'Week 45' }).waitFor({ timeout: 5000 });
  } finally {
    await day.close();
  }
});

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

await step('an inline reply pops out into a window of its own, keeping what was written', async () => {
  await openConversation('Hafta sonu');
  await w.locator('body').press('r');
  await w.locator('.reader .composer.inline .ProseMirror').click();
  await w.keyboard.type('Cumartesi uyar.');
  // The demo opens it as a page of its own, as the app opens a window.
  const [win] = await Promise.all([w.context().waitForEvent('page'), w.locator('.reader .composer.inline .icon-btn[aria-label="Pop out"]').click()]);
  win.on('pageerror', (e) => problems.push(`window: ${e.message}`));
  try {
    if (!/[?&]window=compose&id=\d+/.test(win.url())) throw new Error(`it opened ${win.url()}`);
    await win.locator('.composer.windowed .ProseMirror', { hasText: 'Cumartesi uyar.' }).waitFor({ timeout: 5000 });
    if (await win.locator('.list, .reader').count()) throw new Error('the window shows more than the message');
    if (await w.locator('.reader .composer.inline').count()) throw new Error('the reply stayed under the conversation');
  } finally {
    await win.close();
  }
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

await step('VIPs: their own mailbox, a crown, is:vip, and a list in Settings', async () => {
  const p = await fresh();
  try {
    // The demo starts with Maya as a VIP.
    const vipBox = p.locator('.sidebar .item', { hasText: 'VIPs' });
    await vipBox.click();
    await p.locator('.list h1', { hasText: 'VIPs' }).waitFor({ timeout: 5000 });
    const subjects = async () => (await p.locator('.row .subject').allInnerTexts()).map((s) => s.trim()).sort();
    await eventually(p, async () => (await subjects()).includes('New onboarding flow — first look'), async () => `the VIPs list has ${JSON.stringify(await subjects())}`);
    if ((await subjects()).includes('Interview loop for Senior Rust Engineer')) throw new Error('Daniel is listed before he is a VIP');
    // Daniel becomes one from his message's menu, and wears a crown.
    await p.locator('.sidebar .item', { hasText: 'Inbox' }).first().click();
    await p.locator('.row', { hasText: 'Interview loop for Senior Rust Engineer' }).click();
    const card = p.locator('.card').filter({ has: p.locator('.row1 .name', { hasText: 'Daniel Okafor' }) });
    await card.locator('.icon-btn[title="More"]').click();
    await p.locator('.menu-item', { hasText: 'Add to VIPs' }).click();
    await toast(p, 'Daniel Okafor is a VIP');
    await card.locator('.row1 .vip').waitFor({ timeout: 5000 });
    await vipBox.click();
    await eventually(p, async () => (await subjects()).includes('Interview loop for Senior Rust Engineer'), async () => `the VIPs list has ${JSON.stringify(await subjects())}`);
    // is:vip finds the same conversations.
    const box = p.locator('.list .search input');
    await box.fill('is:vip');
    await box.press('Enter');
    await eventually(
      p,
      async () => {
        const found = await subjects();
        return found.includes('New onboarding flow — first look') && found.includes('Interview loop for Senior Rust Engineer') && !found.includes('Photos from the weekend 📸') && !found.includes('Apartment viewing on Saturday');
      },
      async () => `is:vip found ${JSON.stringify(await subjects())}`,
    );
    // Settings → Notifications lists them, and removes one.
    await p.keyboard.press('Escape');
    await p.keyboard.press('Control+,');
    await p.locator('.window .tab', { hasText: 'Notifications' }).click();
    const daniel = p.locator('.blocked', { hasText: 'daniel' });
    await daniel.locator('button', { hasText: 'Remove' }).click();
    await toast(p, 'Daniel Okafor is no longer a VIP');
    await daniel.waitFor({ state: 'detached', timeout: 5000 });
    await p.locator('.blocked', { hasText: 'maya@northwind.design' }).waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('Subscriptions: unsubscribe, archive what a list left in the inbox, delete all of it', async () => {
  const p = await fresh();
  try {
    // From the Newsletters tab of the inbox.
    await p.locator('.tabs [role="tab"]', { hasText: 'Newsletters' }).click();
    await p.locator('.list .tools button[aria-label="Manage subscriptions"]').click();
    const dialog = p.locator('[role="dialog"]', { has: p.locator('h3', { hasText: 'Subscriptions' }) });
    const byte = dialog.locator('.row', { hasText: 'The Daily Byte' });
    await byte.waitFor({ timeout: 5000 });
    if (!(await dialog.locator('.row', { hasText: 'Rust Weekly' }).count())) throw new Error('Rust Weekly is not listed');
    if (await dialog.locator('.row', { hasText: 'Maya Chen' }).count()) throw new Error('a person is listed');
    await byte.locator('button', { hasText: 'Unsubscribe' }).click();
    await toast(p, 'Unsubscribed from The Daily Byte');
    await byte.locator('.done', { hasText: 'Unsubscribed' }).waitFor({ timeout: 5000 });
    // It keeps its place, not to move what is under the pointer.
    if ((await dialog.locator('.row').first().innerText()).indexOf('The Daily Byte') < 0) throw new Error('the row moved');
    // What it left in the inbox goes to Archive, and comes back with Undo.
    await byte.locator('button[aria-label="Archive 1 conversation from the inbox"]').click();
    await toast(p, 'Archived 1 conversation from The Daily Byte');
    await eventually(p, async () => (await byte.locator('button[aria-label^="Archive"]').count()) === 0, 'the archive button stayed');
    await p.locator('.toast', { hasText: 'Archived 1 conversation' }).first().locator('.action').click();
    await byte.locator('button[aria-label="Archive 1 conversation from the inbox"]').waitFor({ timeout: 5000 });
    // Deleting all of a sender's mail asks first.
    const rust = dialog.locator('.row', { hasText: 'Rust Weekly' });
    await rust.locator('button[aria-label="More for Rust Weekly"]').click();
    await p.locator('.menu-item', { hasText: 'Delete all' }).click();
    const ask = p.locator('[role="alertdialog"]');
    await ask.locator('h3', { hasText: 'Delete all mail from Rust Weekly?' }).waitFor({ timeout: 5000 });
    await ask.locator('button.danger').click();
    await toast(p, 'Moved 1 conversation to Trash');
    await rust.waitFor({ state: 'detached', timeout: 5000 });
    // The palette opens it too.
    await p.keyboard.press('Escape');
    await dialog.waitFor({ state: 'detached', timeout: 5000 });
    await p.keyboard.press('Control+k');
    await p.keyboard.type('Manage subscriptions');
    await p.keyboard.press('Enter');
    await dialog.waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('a newer version is announced in the sidebar, and the notice can be put away', async () => {
  const p = await fresh({ 'envelope.demo.release': '1' });
  try {
    await p.keyboard.press('Control+,');
    await p.locator('.window .tab', { hasText: 'About' }).click();
    await p.locator('.window button', { hasText: 'Check now' }).click();
    await toast(p, 'Envelope 0.9.0 is available');
    await p.locator('.window .newer', { hasText: 'Envelope 0.9.0 is available' }).waitFor({ timeout: 5000 });
    await p.keyboard.press('Escape');
    const banner = p.locator('.sidebar .banner', { hasText: 'Envelope 0.9.0 is available' });
    await banner.waitFor({ timeout: 5000 });
    await banner.locator('button[aria-label="Not now"]').click();
    await banner.waitFor({ state: 'detached', timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('mail marked important shows a red mark, and the composer marks a message important', async () => {
  const p = await fresh();
  try {
    const row = p.locator('.row', { hasText: 'Q4 roadmap — final review' });
    await row.locator('.important').waitFor({ timeout: 5000 });
    if ((await p.locator('.row .important').count()) !== 1) throw new Error(`${await p.locator('.row .important').count()} rows are marked important`);
    await row.click();
    await p.locator('.reader .card .importance', { hasText: 'High importance' }).waitFor({ timeout: 5000 });
    // A new message, marked important from the footer, then low from the send menu.
    await p.locator('body').press('c');
    const composer = p.locator('.composer');
    await composer.waitFor({ timeout: 5000 });
    const toggle = composer.locator('button.importance');
    await toggle.click();
    if ((await toggle.getAttribute('aria-pressed')) !== 'true') throw new Error('the importance toggle is not on');
    await composer.locator('button[aria-label="Send later"], .send-more').first().click();
    const high = p.locator('.popover .followup button', { hasText: 'High' });
    if (!(await high.evaluate((el) => el.classList.contains('on')))) throw new Error('High is not chosen in the send menu');
    await p.locator('.popover .followup button', { hasText: 'Low' }).click();
    if ((await toggle.getAttribute('aria-pressed')) !== 'false') throw new Error('Low importance left the toggle on');
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

await step('a contact group typed in To adds its people, each once', async () => {
  const p = await fresh();
  try {
    await p.locator('body').press('c');
    const to = p.locator('.dock .composer .chips input').first();
    await to.waitFor({ timeout: 5000 });
    // Maya is there already: the group adds the other four.
    await to.fill('maya@northwind.design');
    await p.keyboard.press('Enter');
    await to.fill('design');
    const option = p.locator('.suggest .opt', { hasText: 'Design team' });
    await option.waitFor({ timeout: 5000 });
    if (!(await option.innerText()).includes('5 people')) throw new Error(`the group is offered as "${await option.innerText()}"`);
    if (!(await option.locator('svg').count())) throw new Error('the group has no icon');
    await p.keyboard.press('Enter');
    const chips = p.locator('.dock .composer .field').first().locator('.chip-addr');
    await eventually(p, async () => (await chips.count()) === 5, async () => `To holds ${await chips.count()} addresses`);
    const names = await chips.allInnerTexts();
    for (const name of ['Sofia Martínez', 'Priya Raman', 'Daniel Okafor', 'Olivia Brooks']) {
      if (!names.some((n) => n.includes(name))) throw new Error(`${name} is missing: ${names.join(', ')}`);
    }
  } finally {
    await p.close();
  }
});

await step('a contact group is made, renamed, filled, emptied and deleted in Contacts', async () => {
  const p = await fresh();
  try {
    await p.locator('.modes button[aria-label="Contacts"]').click();
    await p.locator('.contacts .row').first().waitFor({ timeout: 5000 });
    const group = (name) => p.locator('.sidebar .item.in-book', { hasText: name });
    if ((await group('Design team').locator('.count').innerText()) !== '5') throw new Error('Design team does not count 5 people');
    // A new group of the iCloud address book; a name another group there has is refused first.
    const book = p.locator('.sidebar .book', { hasText: 'iCloud' });
    await book.hover();
    await book.locator('.add-group').click();
    const prompt = p.locator('[role="dialog"] input');
    await prompt.waitFor({ timeout: 5000 });
    await p.keyboard.type('family');
    await p.keyboard.press('Enter');
    await p.locator('[role="dialog"] .message', { hasText: 'already a group called “family”' }).waitFor({ timeout: 5000 });
    await prompt.fill('Climbing');
    await p.keyboard.press('Enter');
    await toast(p, 'Group “Climbing” created');
    // Then people to add: the address book's contacts.
    const picker = p.locator('[role="dialog"]', { hasText: 'Add people to “Climbing”' });
    await picker.waitFor({ timeout: 5000 });
    for (const name of ['Zoe Garcia', 'Ben Cohen']) await picker.locator('label.person', { hasText: name }).click();
    await picker.locator('button.primary', { hasText: 'Add 2 people' }).click();
    await toast(p, 'Added 2 people to “Climbing”');
    const members = p.locator('.contacts .list .member');
    await eventually(p, async () => (await members.count()) === 2 && (await group('Climbing').locator('.count').innerText()) === '2', 'Climbing does not hold 2 people');
    // Renamed from the group's menu.
    await p.locator('.group-bar .group-menu').click();
    await p.locator('.popover .menu-item', { hasText: 'Rename…' }).click();
    await prompt.waitFor({ timeout: 5000 });
    await prompt.fill('Bouldering');
    await p.keyboard.press('Enter');
    await toast(p, 'Renamed to “Bouldering”');
    await p.locator('.contacts .list header h1', { hasText: 'Bouldering' }).waitFor({ timeout: 5000 });
    // A contact's page shows the groups it is in.
    await members.filter({ hasText: 'Zoe Garcia' }).locator('.row').click();
    await p.locator('.contacts .detail .group', { hasText: 'Bouldering' }).waitFor({ timeout: 5000 });
    // Taken out of the group from its list.
    const ben = members.filter({ hasText: 'Ben Cohen' });
    await ben.hover();
    await ben.locator('.remove').click();
    await toast(p, 'Removed Ben Cohen from “Bouldering”');
    await eventually(p, async () => (await members.count()) === 1, 'Ben Cohen is still in the group');
    // Deleting asks first, and the people stay.
    await p.locator('.group-bar .group-menu').click();
    await p.locator('.popover .menu-item', { hasText: 'Delete group…' }).click();
    const ask = p.locator('[role="alertdialog"]');
    await ask.locator('h3', { hasText: 'Delete group “Bouldering”?' }).waitFor({ timeout: 5000 });
    await ask.locator('.message', { hasText: 'The person in it stays in your contacts.' }).waitFor({ timeout: 5000 });
    await ask.locator('button.danger').click();
    await toast(p, 'Deleted “Bouldering”');
    await eventually(p, async () => !(await group('Bouldering').count()), 'Bouldering is still listed');
    await p.locator('.contacts .row', { hasText: 'Zoe Garcia' }).waitFor({ timeout: 5000 });
    // A read-only group can still be written to.
    await group('Design team').click();
    await p.locator('.group-bar .note', { hasText: 'Read-only' }).waitFor({ timeout: 5000 });
    if (await p.locator('.group-bar button', { hasText: 'Add people…' }).count()) throw new Error('a read-only group offers Add people…');
    await p.locator('.group-bar button', { hasText: 'Write to group' }).click();
    await eventually(p, async () => (await p.locator('.dock .composer .field').first().locator('.chip-addr').count()) === 5, 'Write to group did not address the 5 people');
  } finally {
    await p.close();
  }
});

await step('messages from an mbox file are imported into a folder', async () => {
  const p = await open();
  try {
    await p.goto(url);
    await p.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
    await p.locator('.sidebar .item', { hasText: 'Receipts' }).first().click({ button: 'right' });
    await p.locator('.menu-item', { hasText: 'Import messages' }).click();
    await p.locator('.toast', { hasText: 'Importing into Receipts' }).first().waitFor({ timeout: 5000 });
    await p.locator('.toast', { hasText: '62 messages imported into Receipts · 3 were already there' }).first().waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

// Repeats and time zones, in a window on Istanbul time on Tuesday 10 November 2026 at 08:00,
// the month's second Tuesday.
const z = await open(Date.UTC(2026, 10, 10, 5, 0), { timezoneId: 'Europe/Istanbul' });
await z.goto(url);
await z.locator('.row').first().waitFor({ state: 'visible', timeout: 5000 });
await z.locator('.modes button', { hasText: 'Calendar' }).click();
await z.locator('.grid-wrap').waitFor({ timeout: 5000 });
const zEditor = z.locator('.editor[role="dialog"]');
/** The event editor's repeat menu, and what it says. */
const repeatMenu = zEditor.locator('select:has(option[value="custom"])');
const repeatText = () => repeatMenu.evaluate((s) => s.options[s.selectedIndex].text);

await step('a custom repeat: the second Tuesday of every month, five times', async () => {
  await z.locator('body').press('c');
  await zEditor.waitFor({ timeout: 5000 });
  await z.keyboard.type('Board meeting');
  await repeatMenu.selectOption('custom');
  const dialog = z.locator('[role="dialog"][aria-label="Custom repeat"]');
  await dialog.waitFor({ timeout: 5000 });
  await dialog.locator('select.unit').selectOption('MONTHLY');
  // What a monthly repeat can follow, from the start date.
  const monthly = dialog.locator('select.monthly');
  const offered = (await monthly.locator('option').allInnerTexts()).join(' | ');
  if (offered !== 'Monthly on day 10 | Monthly on the second Tuesday') throw new Error(`it offers ${offered}`);
  await monthly.selectOption('2TU');
  await dialog.locator('input[type="radio"][value="count"]').check();
  await dialog.locator('.ends input.num').fill('5');
  const summary = (await dialog.locator('.summary').innerText()).trim();
  if (summary !== 'Monthly on the second Tuesday, 5 times') throw new Error(`the dialog reads "${summary}"`);
  await dialog.locator('button', { hasText: 'OK' }).click();
  await dialog.waitFor({ state: 'detached', timeout: 5000 });
  if ((await repeatText()) !== 'Monthly on the second Tuesday, 5 times') throw new Error(`the editor says "${await repeatText()}"`);
  await zEditor.locator('footer .btn.primary').click();
  await zEditor.waitFor({ state: 'detached', timeout: 5000 });
  // Read back in the popover…
  await z.locator('.ev', { hasText: 'Board meeting' }).first().click();
  const line = z.locator('.pop .line', { hasText: /Monthly/ }).first();
  await eventually(z, async () => (await line.count()) > 0 && (await line.innerText()).trim() === 'Monthly on the second Tuesday, 5 times', async () => `the popover says "${(await line.count()) ? await line.innerText() : 'nothing'}"`);
  await z.keyboard.press('Escape');
  // …and on the calendar: the second Tuesdays from November to March.
  await z.locator('body').press('m');
  const days = () => z.locator('.cell:not(.other)').evaluateAll((cells) => cells.filter((c) => c.innerText.includes('Board meeting')).map((c) => c.querySelector('.num').textContent.trim()).join(','));
  for (const expected of ['10', '8', '12', '9', '9', '']) {
    await eventually(z, async () => (await days()) === expected, async () => `${await z.locator('.calendar h1').innerText()} has it on "${await days()}", not "${expected}"`);
    await z.locator('body').press('j');
  }
  await z.locator('body').press('t');
  await z.locator('body').press('w');
});

await step('an event planned in another zone shows at its time here, and says so', async () => {
  await z.locator('body').press('c');
  await zEditor.waitFor({ timeout: 5000 });
  await z.keyboard.type('Call with New York');
  await zEditor.locator('button.tz').click();
  await z.locator('.picker input').fill('new york');
  await z.keyboard.press('Enter');
  const zone = (await zEditor.locator('button.tz').innerText()).trim();
  if (!zone.startsWith('New York')) throw new Error(`the zone reads "${zone}"`);
  // 10:00 in New York is 18:00 in Istanbul.
  const start = zEditor.locator('.dt input').first();
  await start.fill('10:00');
  await start.press('Enter');
  await zEditor.locator('footer .btn.primary').click();
  await zEditor.waitFor({ state: 'detached', timeout: 5000 });
  await z.locator('.ev', { hasText: 'Call with New York' }).first().click();
  await z.locator('.pop .line.zone').waitFor({ timeout: 5000 });
  const when = await z.locator('.pop .line').first().innerText();
  if (!/6:00\sPM\s–\s7:00\sPM/.test(when)) throw new Error(`it is at "${when}" here`);
  const there = await z.locator('.pop .line.zone').innerText();
  if (!/^Eastern Time: 10:00\sAM\s–\s11:00\sAM$/.test(there.trim())) throw new Error(`the popover says "${there}"`);
  // The editor opens in that zone.
  await z.locator('.pop button[title="Edit"]').click();
  await zEditor.waitFor({ timeout: 5000 });
  if (!(await zEditor.locator('button.tz').innerText()).startsWith('New York')) throw new Error('the editor lost the zone');
  const typed = await zEditor.locator('.dt input').first().inputValue();
  if (!/^10:00\sAM$/.test(typed)) throw new Error(`the editor starts it at ${typed}`);
  await z.keyboard.press('Escape');
  await zEditor.waitFor({ state: 'detached', timeout: 5000 });
});

await step('a second time zone gets an hour column of its own', async () => {
  await z.keyboard.press('Control+,');
  const settings = z.locator('.window[role="dialog"]');
  await settings.waitFor({ timeout: 5000 });
  await settings.locator('.tab', { hasText: 'Calendar' }).click();
  await settings.locator('.row', { hasText: 'Show a second time zone' }).locator('[role="switch"]').click();
  await settings.locator('button.tz').click();
  await z.locator('.picker input').fill('tokyo');
  await z.keyboard.press('Enter');
  await z.keyboard.press('Escape');
  await settings.waitFor({ state: 'detached', timeout: 5000 });
  await z.locator('.grid-wrap.zones').waitFor({ timeout: 5000 });
  const labels = (await z.locator('.zone-label').allInnerTexts()).join(' ');
  if (labels !== 'GMT+9 GMT+3') throw new Error(`the columns are labelled "${labels}"`);
  // 9 in the morning here is 3 in the afternoon in Tokyo.
  const nine = (await z.locator('.hour-label.second').nth(9).innerText()).trim();
  if (nine !== '3 PM') throw new Error(`9 AM reads "${nine}" in Tokyo`);
});

await step('the work week view shows Monday to Friday, from its key and the palette', async () => {
  const weekdays = () => z.locator('.dayhead .dow').allInnerTexts().then((d) => d.join(' ').toUpperCase());
  await z.locator('body').press('x');
  await eventually(z, async () => (await weekdays()) === 'MON TUE WED THU FRI', async () => `the view shows ${await weekdays()}`);
  if ((await z.getByRole('tab', { name: 'Work week', exact: true }).getAttribute('aria-selected')) !== 'true') throw new Error('the Work week tab is not on');
  await z.locator('body').press('w');
  await eventually(z, async () => (await z.locator('.dayhead').count()) === 7, 'w did not go back to the week');
  await z.keyboard.press('Control+k');
  await z.locator('.palette input').waitFor({ timeout: 5000 });
  await z.keyboard.type('work week');
  await z.keyboard.press('Enter');
  await eventually(z, async () => (await weekdays()) === 'MON TUE WED THU FRI', async () => `the palette showed ${await weekdays()}`);
});
await z.close();

/** Whether the focus is inside `locator`'s element. */
const holdsFocus = (locator) => locator.evaluate((el) => el.contains(document.activeElement));

await step('dialogs take the focus, keep Tab inside and give the focus back', async () => {
  const p = await fresh();
  try {
    const list = p.getByRole('listbox', { name: 'Conversations' });
    await p.locator('.row').first().click();
    if (!(await holdsFocus(list))) throw new Error('a clicked row did not give the list the focus');
    for (const [name, open] of [
      ['Settings', () => p.keyboard.press('Control+,')],
      ['Keyboard shortcuts', () => p.keyboard.press('?')],
      ['Command palette', () => p.keyboard.press('Control+k')],
    ]) {
      await open();
      const dialog = p.getByRole('dialog', { name });
      await dialog.waitFor({ timeout: 5000 });
      if ((await dialog.getAttribute('aria-modal')) !== 'true') throw new Error(`${name} is not modal`);
      await eventually(p, () => holdsFocus(dialog), `${name} did not take the focus`);
      for (let i = 0; i < 25; i++) {
        await p.keyboard.press(i % 4 === 3 ? 'Shift+Tab' : 'Tab');
        if (!(await holdsFocus(dialog))) throw new Error(`Tab left ${name} after ${i + 1} presses`);
      }
      await p.keyboard.press('Escape');
      await dialog.waitFor({ state: 'detached', timeout: 5000 });
      await eventually(p, () => holdsFocus(list), `closing ${name} did not give the list the focus back`);
    }
  } finally {
    await p.close();
  }
});

await step('every switch and dropdown in Settings and the rule and contact editors has a name', async () => {
  const p = await fresh();
  try {
    await p.keyboard.press('Control+,');
    const settings = p.getByRole('dialog', { name: 'Settings' });
    await settings.waitFor({ timeout: 5000 });
    const unnamed = [];
    // Controls without a name read "- switch [checked]" or "- combobox:" in the accessibility tree.
    const look = async (where, locator) => {
      for (const line of (await locator.ariaSnapshot()).split('\n')) if (/^\s*- (switch|combobox|textbox)\b(?! ")/.test(line)) unnamed.push(`${where}: ${line.trim()}`);
    };
    for (const tab of await settings.locator('.tab').allInnerTexts()) {
      await settings.locator('.tab', { hasText: tab }).click();
      await p.waitForTimeout(150);
      await look(tab, settings);
    }
    // The rule editor too.
    await settings.locator('.tab', { hasText: 'Rules' }).click();
    await settings.locator('.btn', { hasText: 'New rule' }).click();
    await look('a new rule', settings.locator('.editor-card'));
    await p.keyboard.press('Escape');
    // And the contact editor.
    await p.locator('body').press('g');
    await p.locator('body').press('p');
    await p.locator('.contacts .row', { hasText: 'Maya Chen' }).click();
    await p.locator('body').press('e');
    const editor = p.locator('.contacts .editor');
    await editor.waitFor({ timeout: 5000 });
    await look('a contact', editor);
    if (unnamed.length) throw new Error(unnamed.join('; '));
  } finally {
    await p.close();
  }
});

await step('the list tells which conversation is selected, and row buttons are reached only on it', async () => {
  const p = await fresh();
  try {
    const list = p.getByRole('listbox', { name: 'Conversations' });
    await p.locator('body').press('j');
    await p.locator('body').press('j');
    await eventually(p, () => holdsFocus(list), 'j did not give the list the focus');
    const selected = list.getByRole('option', { selected: true });
    if ((await selected.count()) !== 1) throw new Error(`${await selected.count()} conversations are selected`);
    const active = await list.getAttribute('aria-activedescendant');
    if (!active || active !== (await selected.getAttribute('id'))) throw new Error(`the list points at ${active}, not the selected row`);
    const subject = (await p.locator('.reader .subject-block h2').innerText()).trim();
    if (!(await selected.innerText()).includes(subject)) throw new Error(`the selected row is not "${subject}"`);
    // Only the selected row's buttons are in the tab order, and its actions show once one has the focus.
    const reachable = await p.locator('.row button:not([tabindex="-1"])').evaluateAll((els) => els.map((b) => b.closest('.row')?.getAttribute('aria-selected')));
    if (!reachable.length || reachable.some((s) => s !== 'true')) throw new Error('buttons of rows that are not selected can be tabbed to');
    await p.keyboard.press('Tab');
    await p.keyboard.press('Tab');
    const archive = selected.locator('.actions button').first();
    if (!(await archive.evaluate((b) => b === document.activeElement))) throw new Error('Tab did not reach the row\'s actions');
    const shown = async () => Number(await selected.locator('.actions').evaluate((el) => getComputedStyle(el).opacity)) === 1;
    await eventually(p, shown, 'the focused actions stay hidden');
  } finally {
    await p.close();
  }
});

await step('the composer names its From and Subject fields', async () => {
  const p = await fresh();
  try {
    await p.locator('body').press('c');
    const composer = p.locator('.dock .composer');
    await composer.getByRole('combobox', { name: 'From' }).waitFor({ timeout: 5000 });
    await composer.getByRole('textbox', { name: 'Subject' }).waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('menus take the keyboard: arrows, Home, End, Tab and Escape stay in them', async () => {
  const p = await fresh();
  try {
    const list = p.getByRole('listbox', { name: 'Conversations' });
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    const subject = () => p.locator('.reader .subject-block h2').innerText();
    const before = await subject();
    const menu = p.locator('.popover');
    const items = menu.locator('.menu-item');
    const focused = () => p.evaluate(() => document.activeElement?.textContent?.trim() ?? '');
    const on = async (i) => (await focused()) === (await items.nth(i).textContent()).trim();
    // The snooze menu (h) takes the focus; the arrows move in it and Enter chooses.
    await p.locator('body').press('h');
    await menu.waitFor({ timeout: 5000 });
    await eventually(p, () => on(0), 'the snooze menu did not take the focus');
    await p.keyboard.press('ArrowDown');
    if (!(await on(1))) throw new Error(`ArrowDown is on "${await focused()}"`);
    await p.keyboard.press('Escape');
    await menu.waitFor({ state: 'detached', timeout: 5000 });
    await eventually(p, () => holdsFocus(list), 'Escape did not give the list the focus back');
    // The context menu (Shift+F10): Home, End and Tab stay in it, and nothing reaches the list.
    await p.keyboard.press('Shift+F10');
    await menu.waitFor({ timeout: 5000 });
    await eventually(p, () => on(0), 'the context menu did not take the focus');
    await p.keyboard.press('End');
    if (!(await on((await items.count()) - 1))) throw new Error(`End is on "${await focused()}"`);
    await p.keyboard.press('Home');
    if (!(await on(0))) throw new Error(`Home is on "${await focused()}"`);
    await p.keyboard.press('ArrowUp');
    if (!(await on((await items.count()) - 1))) throw new Error(`ArrowUp from the top is on "${await focused()}"`);
    for (let i = 0; i < 20; i++) await p.keyboard.press(i % 3 ? 'Tab' : 'Shift+Tab');
    if (!(await menu.evaluate((m) => m.contains(document.activeElement)))) throw new Error('Tab left the menu');
    await p.keyboard.press('j');
    if ((await subject()) !== before) throw new Error('keys in the menu moved the list behind it');
    await p.keyboard.press('Escape');
    await menu.waitFor({ state: 'detached', timeout: 5000 });
    // Enter chooses.
    await p.locator('body').press('h');
    await menu.waitFor({ timeout: 5000 });
    await p.keyboard.press('ArrowDown');
    await p.keyboard.press('Enter');
    await toast(p, 'Snoozed until');
    await menu.waitFor({ state: 'detached', timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('Shift+F10 and the Menu key open the context menu of the selected conversation', async () => {
  const p = await fresh();
  try {
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    await p.keyboard.press('Shift+F10');
    const reply = p.locator('.popover .menu-item', { hasText: 'Reply all' });
    await reply.waitFor({ timeout: 5000 });
    await p.keyboard.press('ArrowDown');
    if (!(await reply.evaluate((b) => b === document.activeElement))) throw new Error('the arrow did not move to Reply all');
    await p.keyboard.press('Escape');
    await reply.waitFor({ state: 'detached', timeout: 5000 });
    await p.keyboard.press('ContextMenu');
    await reply.waitFor({ timeout: 5000 });
    await p.keyboard.press('Escape');
  } finally {
    await p.close();
  }
});

await step('e in Contacts edits without typing an e', async () => {
  const p = await fresh();
  try {
    await p.locator('body').press('g');
    await p.locator('body').press('p');
    await p.locator('.contacts .row', { hasText: 'Maya Chen' }).click();
    await p.locator('body').press('e');
    const first = p.locator('.contacts .editor input[placeholder="First name"]');
    await first.waitFor({ timeout: 5000 });
    await p.waitForTimeout(200);
    if ((await first.inputValue()) !== 'Maya') throw new Error(`the first name reads "${await first.inputValue()}"`);
  } finally {
    await p.close();
  }
});

await step('Space, Page Down, Home and End read a conversation; Space at its end opens the next unread', async () => {
  const p = await fresh();
  try {
    await p.setViewportSize({ width: 1280, height: 560 });
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    const scroll = p.locator('.reader .scroll');
    await p.locator('.reader .card').first().waitFor({ timeout: 5000 });
    // Every message unfolded (;), so that it is long enough to scroll.
    await p.keyboard.press(';');
    const top = () => scroll.evaluate((el) => el.scrollTop);
    const atEnd = () => scroll.evaluate((el) => el.scrollTop >= el.scrollHeight - el.clientHeight - 2);
    await eventually(p, async () => !(await atEnd()), 'the conversation is not long enough to scroll');
    await p.keyboard.press('Space');
    await eventually(p, async () => (await top()) > 100, 'Space did not scroll the conversation');
    const down = await top();
    await p.keyboard.press('Shift+Space');
    await eventually(p, async () => (await top()) < down, 'Shift+Space did not scroll back');
    await p.keyboard.press('End');
    await eventually(p, atEnd, 'End did not go to the end');
    await p.keyboard.press('Home');
    await eventually(p, async () => (await top()) === 0, 'Home did not go to the top');
    await p.keyboard.press('PageDown');
    await eventually(p, async () => (await top()) > 100, 'Page Down did not scroll');
    // At the end, Space opens the next unread conversation below.
    const next = (await p.locator('.row.selected ~ .row.unread .subject').first().innerText()).trim();
    await p.keyboard.press('End');
    await eventually(p, atEnd, 'End did not go to the end');
    await p.keyboard.press('Space');
    await p.locator('.reader .subject-block h2', { hasText: next }).waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('n and p move between messages, and ; expands them all', async () => {
  const p = await fresh();
  try {
    await p.locator('.row', { hasText: 'Offsite planning' }).first().click();
    const cards = p.locator('.reader article.card');
    await cards.first().waitFor({ timeout: 5000 });
    const current = () => p.evaluate(() => document.activeElement?.closest('[data-message]')?.getAttribute('data-message') ?? null);
    const ids = () => p.locator('.reader [data-message]').evaluateAll((els) => els.map((el) => el.getAttribute('data-message')));
    await p.keyboard.press('n');
    await eventually(p, async () => (await current()) === (await ids())[1], 'n did not move to the second message');
    if ((await p.locator('.reader .more').count()) > 0) throw new Error('the folded messages stayed folded');
    await p.keyboard.press('n');
    await eventually(p, async () => (await current()) === (await ids())[2], 'n did not move to the third message');
    await p.keyboard.press('p');
    await eventually(p, async () => (await current()) === (await ids())[1], 'p did not move back');
    await p.keyboard.press(';');
    await eventually(p, async () => (await cards.count()) === (await ids()).length, 'not every message was expanded');
    // In the command palette too.
    await p.keyboard.press('Control+k');
    await p.keyboard.type('next message');
    await p.locator('.palette .row').filter({ has: p.locator('.label', { hasText: /^Next message$/ }) }).click();
    await eventually(p, async () => (await current()) === (await ids())[2], 'the palette did not move to the next message');
  } finally {
    await p.close();
  }
});

await step('Escape leaves a reply for the conversation, and closes it the second time', async () => {
  const p = await fresh();
  try {
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    await p.locator('.reader .subject-block h2').waitFor({ timeout: 5000 });
    await p.locator('body').press('r');
    const inline = p.locator('.reader .composer.inline');
    await inline.locator('.ProseMirror').click();
    await p.keyboard.type('Sounds good, see you at ten.');
    await p.keyboard.press('Escape');
    if (!(await inline.count())) throw new Error('the first Escape closed the reply');
    await eventually(p, () => p.locator('.reader .scroll').evaluate((el) => el === document.activeElement), 'the conversation did not get the focus');
    await p.keyboard.press('Escape');
    await inline.waitFor({ state: 'detached', timeout: 5000 });
    await toast(p, 'Draft saved');
    // The shortcuts sheet lists the reading keys.
    await p.keyboard.press('?');
    const sheet = p.getByRole('dialog', { name: 'Keyboard shortcuts' });
    for (const text of ['Reading', 'Expand all messages', 'Next / previous message', 'Menu of the selected conversation']) await sheet.getByText(text, { exact: true }).waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('the search box shows the search the list is showing', async () => {
  const p = await fresh();
  try {
    const box = p.locator('.list .search input');
    await box.fill('roadmap');
    await p.locator('.list h1', { hasText: 'Search' }).waitFor({ timeout: 5000 });
    // Another folder drops the search, and the box with it.
    await p.locator('.sidebar .item', { hasText: 'Sent' }).first().click();
    await p.locator('.list h1', { hasText: 'Sent' }).waitFor({ timeout: 5000 });
    if (await box.inputValue()) throw new Error(`the box still reads "${await box.inputValue()}"`);
    // A search started elsewhere (a message found in the command palette) shows in it.
    await p.keyboard.press('Control+k');
    await p.keyboard.type('Offsite');
    await p.locator('.palette .row', { hasText: 'Offsite planning' }).first().click();
    await eventually(p, async () => (await box.inputValue()) === 'Offsite', async () => `the box reads "${await box.inputValue()}"`);
  } finally {
    await p.close();
  }
});

await step('the search options write a query; results say where they are and the search is remembered', async () => {
  const p = await fresh();
  try {
    const box = p.locator('.list .search input');
    await p.locator('.list .search .opts').click();
    const panel = p.locator('.list .search .panel');
    await panel.locator('label.field', { hasText: 'From' }).locator('input').fill('priya');
    await panel.locator('label', { hasText: 'Has attachment' }).locator('input').check();
    await panel.locator('label.field', { hasText: 'Date' }).locator('select').selectOption({ label: 'Last 30 days' });
    await panel.locator('button', { hasText: 'Search' }).click();
    await eventually(p, async () => (await box.inputValue()) === 'from:priya has:attachment newer_than:1m', async () => `the box reads "${await box.inputValue()}"`);
    const row = p.locator('.row', { hasText: 'Q4 roadmap — final review' });
    await row.waitFor({ timeout: 5000 });
    if ((await p.locator('.row').count()) !== 1) throw new Error(`${await p.locator('.row').count()} results`);
    if ((await row.locator('.place').innerText()).trim() !== 'Inbox') throw new Error(`the result says "${await row.locator('.place').innerText()}"`);
    if (await panel.count()) throw new Error('the options stayed open');
    // Cleared and back in the box: the search is among the recent ones, and runs again from there.
    await box.press('Escape');
    await box.focus();
    const recent = panel.locator('.item', { hasText: 'from:priya has:attachment newer_than:1m' });
    await recent.waitFor({ timeout: 5000 });
    await box.press('ArrowDown');
    if (!(await recent.evaluate((el) => el === document.activeElement))) throw new Error('Down did not go into the recent searches');
    await p.keyboard.press('Enter');
    await eventually(p, async () => (await box.inputValue()) === 'from:priya has:attachment newer_than:1m', 'the recent search did not run');
    await row.waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('the search panel goes with the focus, Escape keeps what was typed, and the options read the search', async () => {
  const p = await fresh();
  try {
    const box = p.locator('.list .search input');
    const panel = p.locator('.list .search .panel');
    // Open options close when a conversation is opened (one below them).
    await p.locator('.list .search .opts').click();
    await panel.waitFor({ timeout: 5000 });
    await p.locator('.row').nth(7).click();
    await panel.waitFor({ state: 'detached', timeout: 5000 });
    // Escape puts the suggestions away first, keeping the text.
    await box.fill('roadmap from:may');
    await panel.locator('.item', { hasText: 'Maya Chen' }).waitFor({ timeout: 5000 });
    await box.press('Escape');
    await panel.waitFor({ state: 'detached', timeout: 5000 });
    if ((await box.inputValue()) !== 'roadmap from:may') throw new Error(`Escape left "${await box.inputValue()}"`);
    await box.press('Escape');
    if (await box.inputValue()) throw new Error('the second Escape kept the search');
    // The options open on the search in the box.
    await box.fill('from:priya has:attachment budget');
    await box.press('Enter');
    await p.locator('.list .search .opts').click();
    if ((await panel.locator('label.field', { hasText: 'From' }).locator('input').inputValue()) !== 'priya') throw new Error('From is not filled from the search');
    if ((await panel.locator('label.field', { hasText: 'Has the words' }).locator('input').inputValue()) !== 'budget') throw new Error('the words are not filled from the search');
    if (!(await panel.locator('label', { hasText: 'Has attachment' }).locator('input').isChecked())) throw new Error('Has attachment is not ticked');
    await p.keyboard.press('Escape');
    // A saved search shows as saved, and is not saved twice.
    await p.locator('.list .tools button[aria-label="Save search"]').click();
    const prompt = p.locator('[role="dialog"] input');
    await prompt.fill('Budget files');
    await p.keyboard.press('Enter');
    const saved = p.locator('.list .tools button[aria-label="Remove “Budget files” from saved searches"]');
    await saved.waitFor({ timeout: 5000 });
    await saved.click();
    await toast(p, 'Deleted “Budget files”');
    await p.locator('.list .tools button[aria-label="Save search"]').waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('from: in the search box suggests people, and picking one completes it', async () => {
  const p = await fresh();
  try {
    const box = p.locator('.list .search input');
    await box.fill('roadmap from:may');
    const maya = p.locator('.list .search .panel .item', { hasText: 'Maya Chen' });
    await maya.waitFor({ timeout: 5000 });
    await maya.click();
    if ((await box.inputValue()) !== 'roadmap from:maya@northwind.design ') throw new Error(`the box reads "${await box.inputValue()}"`);
    if (!(await box.evaluate((el) => el === document.activeElement))) throw new Error('the box lost the focus');
  } finally {
    await p.close();
  }
});

await step('a search is saved to the sidebar, run from there, renamed and deleted', async () => {
  const p = await fresh();
  try {
    const box = p.locator('.list .search input');
    const saved = (name) => p.locator('.sidebar .item', { hasText: name });
    const prompt = p.locator('[role="dialog"] input');
    await box.fill('is:unread has:attachment');
    await box.press('Enter');
    await p.locator('.list .tools button[aria-label="Save search"]').click();
    await prompt.waitFor({ timeout: 5000 });
    if ((await prompt.inputValue()) !== 'is:unread has:attachment') throw new Error(`the name offered is "${await prompt.inputValue()}"`);
    await prompt.fill('Unread files');
    await p.keyboard.press('Enter');
    await toast(p, '“Unread files” is in the sidebar');
    await p.locator('.sidebar .item', { hasText: 'Sent' }).first().click();
    await p.locator('.list h1', { hasText: 'Sent' }).waitFor({ timeout: 5000 });
    await saved('Unread files').click();
    await eventually(p, async () => (await box.inputValue()) === 'is:unread has:attachment', async () => `the box reads "${await box.inputValue()}"`);
    if (!(await saved('Unread files').evaluate((el) => el.classList.contains('active')))) throw new Error('the saved search is not shown as the one on screen');
    // Its menu, from the keyboard as from the mouse.
    await saved('Unread files').focus();
    await p.keyboard.press('Shift+F10');
    await p.locator('.menu-item', { hasText: 'Rename' }).click();
    await prompt.fill('Files to read');
    await p.keyboard.press('Enter');
    await saved('Files to read').waitFor({ timeout: 5000 });
    await saved('Files to read').click({ button: 'right' });
    await p.locator('.menu-item', { hasText: 'Delete' }).click();
    await toast(p, 'Deleted “Files to read”');
    if (await saved('Files to read').count()) throw new Error('the saved search is still listed');
    await p.locator('.toast', { hasText: 'Deleted “Files to read”' }).first().locator('.action').click();
    await saved('Files to read').waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('another folder closes a conversation it does not list, and keeps one it does', async () => {
  const p = await fresh();
  try {
    const subject = p.locator('.reader .subject-block h2');
    // Starred lists the starred conversation that is open: it stays.
    await p.locator('.row', { hasText: 'New onboarding flow' }).first().click();
    await subject.waitFor({ timeout: 5000 });
    await p.locator('body').press('g');
    await p.locator('body').press('s');
    await p.locator('.list h1', { hasText: 'Starred' }).waitFor({ timeout: 5000 });
    await p.waitForTimeout(400);
    if (!(await subject.innerText()).includes('New onboarding flow')) throw new Error('the conversation Starred lists was closed');
    // Spam doesn't: it goes, and so does one an inbox tab doesn't list.
    await personal(p).locator('.item', { hasText: 'Spam' }).click();
    await p.locator('.reader .placeholder').waitFor({ timeout: 5000 });
    await p.locator('body').press('g');
    await p.locator('body').press('i');
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    await subject.waitFor({ timeout: 5000 });
    await p.locator('body').press('2');
    await p.locator('.reader .placeholder').waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('select all offers every conversation of the folder, and archiving takes them all', async () => {
  // More receipts than a page of the list loads.
  const p = await fresh({ 'envelope.demo.bulk': '220' });
  try {
    await personal(p).locator('.item', { hasText: 'Receipts' }).click();
    await p.locator('.row', { hasText: 'Receipt #' }).first().waitFor({ timeout: 5000 });
    await p.keyboard.press('Control+a');
    const all = p.locator('.bulk .link', { hasText: /^Select all \d+ conversations in this folder$/ });
    await all.waitFor({ timeout: 5000 });
    const count = Number((await all.innerText()).match(/\d+/)[0]);
    const selected = p.locator('.bulk .sel');
    const checked = Number((await selected.innerText()).match(/\d+/)[0]);
    if (!(checked < count) || count < 220) throw new Error(`${checked} of ${count} are checked before asking for all`);
    await all.click();
    await selected.filter({ hasText: `${count} selected` }).waitFor({ timeout: 5000 });
    await p.locator('body').press('e');
    await toast(p, `Archived · ${count} conversations`);
    await eventually(p, async () => (await p.locator('.row').count()) === 0, 'Receipts still lists conversations');
    // Undo brings every one back.
    await p.locator('.toast', { hasText: 'Archived' }).first().locator('.action').click();
    await p.locator('.row', { hasText: 'Receipt #' }).first().waitFor({ timeout: 5000 });
    await p.keyboard.press('Control+a');
    await all.waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('a custom snooze or send time is picked with the app\'s date and time pickers', async () => {
  const p = await fresh();
  try {
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    await p.locator('body').press('h');
    const menu = p.locator('.popover').first();
    await menu.waitFor({ timeout: 5000 });
    if (await p.locator('.popover input[type="datetime-local"]').count()) throw new Error('the browser\'s date field is still there');
    // The 15th of next month, at half past nine in the morning.
    await menu.locator('.later button.field').click();
    await p.locator('.popover .mini button[aria-label="Next month"]').click();
    await p.locator('.popover .mini .d:not(.other)', { hasText: /^15$/ }).click();
    const time = menu.locator('.later input.field');
    await time.fill('9:30');
    await p.keyboard.press('Enter');
    await menu.locator('.later .btn', { hasText: 'Set' }).click();
    await toast(p, '9:30');
    // Sending later offers the same pickers.
    await p.locator('body').press('c');
    await p.locator('.dock .composer .send-more').click();
    await p.locator('.popover .later .btn', { hasText: 'Schedule' }).waitFor({ timeout: 5000 });
    if (await p.locator('.popover input[type="datetime-local"]').count()) throw new Error('sending later still uses the browser\'s date field');
  } finally {
    await p.close();
  }
});

/** Whether every element of `selector` lies within the window's width. */
const inWindow = (p, selector) => p.locator(selector).evaluateAll((els) => els.every((el) => el.getBoundingClientRect().right <= innerWidth + 0.5));

await step('half a screen (960 px): a rail, then the list and the conversation take turns', async () => {
  const p = await fresh();
  try {
    await p.setViewportSize({ width: 960, height: 900 });
    await p.locator('.rail').waitFor({ timeout: 5000 });
    if (await p.locator('.sidebar').count()) throw new Error('the full sidebar is still beside the list');
    const list = p.getByRole('listbox', { name: 'Conversations' });
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    const back = p.locator('.reader .back');
    await back.waitFor({ timeout: 5000 });
    if (await list.count()) throw new Error('the list stays beside the open conversation');
    if (!(await inWindow(p, '.reader header button'))) throw new Error('the toolbar runs out of the window');
    // Escape goes back to the list, with the conversation still selected and the keyboard on it.
    await p.keyboard.press('Escape');
    await list.waitFor({ timeout: 5000 });
    if (await p.locator('.reader').count()) throw new Error('the conversation stays beside the list');
    if (!(await list.getByRole('option', { selected: true }).innerText()).includes('Q4 roadmap')) throw new Error('the conversation is no longer selected');
    await eventually(p, () => holdsFocus(list), 'the list did not get the focus back');
    // j moves in the list without opening; Enter opens, Backspace and the Back button go back.
    await p.keyboard.press('j');
    if (await p.locator('.reader').count()) throw new Error('j in the list opened a conversation');
    await p.keyboard.press('Enter');
    await back.waitFor({ timeout: 5000 });
    await p.keyboard.press('Backspace');
    await list.waitFor({ timeout: 5000 });
    await p.keyboard.press('Enter');
    await back.click();
    await list.waitFor({ timeout: 5000 });
    // The rail opens the whole sidebar over the window; a folder there closes it.
    await p.locator('.rail .expand').click();
    const drawer = p.getByRole('dialog', { name: 'Sidebar' });
    await drawer.waitFor({ timeout: 5000 });
    await eventually(p, () => holdsFocus(drawer), 'the sidebar did not take the focus');
    await p.keyboard.press('Escape');
    await drawer.waitFor({ state: 'detached', timeout: 5000 });
    await p.locator('.rail .expand').click();
    await drawer.locator('.item', { hasText: 'Sent' }).first().click();
    await drawer.waitFor({ state: 'detached', timeout: 5000 });
    await p.locator('.list h1', { hasText: 'Sent' }).waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('the smallest window (780 px): mail, calendar, tasks and contacts fit', async () => {
  const p = await fresh();
  try {
    await p.setViewportSize({ width: 780, height: 560 });
    await p.locator('.row', { hasText: 'New onboarding flow' }).first().click();
    await p.locator('.reader .back').waitFor({ timeout: 5000 });
    if (!(await inWindow(p, '.reader header button, .reader .card header'))) throw new Error('the conversation runs out of the window');
    for (const [keys, sel] of [[['g', 'c'], '.grid-wrap'], [['g', 'k'], '.tasks .quick input'], [['g', 'p'], '.contacts .search input']]) {
      for (const k of keys) await p.locator('body').press(k);
      await p.locator(sel).waitFor({ timeout: 5000 });
      // Nothing to press lies past the window's edge.
      const out = await p.evaluate(() =>
        [...document.querySelectorAll('main button, main input')].filter((el) => {
          const r = el.getBoundingClientRect();
          return r.width > 0 && r.right > innerWidth + 1;
        }).length,
      );
      if (out) throw new Error(`${keys.join(' ')}: ${out} buttons or fields run out of the window`);
    }
    await p.locator('.contacts .row', { hasText: 'Maya Chen' }).click();
    await p.locator('.contacts .detail').waitFor({ timeout: 5000 });
    if (!(await inWindow(p, '.contacts .detail button'))) throw new Error('the contact runs out of the window');
  } finally {
    await p.close();
  }
});

await step('a toolbar short of room puts the rest in More', async () => {
  // Narrower than the window can be made, as with a large text size: the conversation's toolbar
  // has less room than its buttons need.
  const p = await fresh();
  try {
    await p.setViewportSize({ width: 400, height: 700 });
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    const more = p.locator('.reader header .more-btn');
    await more.waitFor({ timeout: 5000 });
    if (!(await inWindow(p, '.reader header button'))) throw new Error('the toolbar runs out of the window');
    await more.click();
    await p.locator('.popover .menu-item', { hasText: 'Label…' }).waitFor({ timeout: 5000 });
    await p.locator('.popover .menu-item', { hasText: 'Move to…' }).click();
    await p.locator('.popover input[placeholder="Move to…"]').waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

/** Opens a conversation of window `p` by searching for its subject. */
async function openIn(p, subject) {
  await p.locator('.search input').fill(subject);
  await p.locator('.row', { hasText: subject }).first().click();
  await p.locator('.reader .subject-block h2', { hasText: subject }).waitFor({ timeout: 5000 });
}

await step('an S/MIME signed message says who signed it, and shows the certificate on request', async () => {
  const p = await fresh();
  try {
    await openIn(p, 'Angebot für das neue Projekt');
    const pill = p.locator('.reader .status .pill', { hasText: 'Signed by Lukas Weber — verified' });
    await pill.waitFor({ timeout: 5000 });
    // The signed message keeps its attachments.
    await p.locator('.reader .tile', { hasText: 'Angebot-Relaunch.pdf' }).waitFor({ timeout: 5000 });
    await pill.click();
    const details = p.locator('.popover .details');
    await details.locator('.row', { hasText: 'Issued by' }).waitFor({ timeout: 5000 });
    const text = await details.innerText();
    for (const part of ['Harbor Trust Email CA', 'lukas.weber@kraftwerk-studio.de', 'Serial number', 'Fingerprint']) {
      if (!text.includes(part)) throw new Error(`the details lack "${part}"`);
    }
    await p.keyboard.press('Escape');
    await details.waitFor({ state: 'detached', timeout: 5000 });
    // Half a screen: the status stays in the window.
    await p.setViewportSize({ width: 960, height: 900 });
    await pill.waitFor({ timeout: 5000 });
    if (!(await inWindow(p, '.reader .status .pill'))) throw new Error('the signature status runs out of the window');
    await pill.click();
    await details.waitFor({ timeout: 5000 });
    if (!(await inWindow(p, '.popover .details'))) throw new Error('the certificate details run out of the window');
  } finally {
    await p.close();
  }
});

await step('S/MIME: certificates in Settings, a .p12 password asked in the app, and S/MIME for a reply', async () => {
  const p = await fresh();
  try {
    await p.keyboard.press('Control+,');
    const settings = p.getByRole('dialog', { name: 'Settings' });
    await settings.waitFor({ timeout: 5000 });
    await settings.locator('.tab', { hasText: 'Encryption' }).click();
    await settings.locator('.item', { hasText: 'alex@lumen.dev' }).filter({ hasText: 'Lumen Corporate CA' }).waitFor({ timeout: 5000 });
    // Lukas's certificate came with his signed mail.
    await settings.locator('.item', { hasText: 'Lukas Weber' }).filter({ hasText: 'From their signed mail' }).waitFor({ timeout: 5000 });
    // The personal address has none: importing one asks for the file's password in the app's own dialog.
    const personal = () => settings.locator('.item', { hasText: 'alex.morgan@icloud.com' });
    await personal().filter({ hasText: 'No certificate' }).locator('.btn', { hasText: 'Import certificate…' }).click();
    await p.locator('[role="dialog"] input[type="password"]:focus').waitFor({ timeout: 5000 });
    await p.keyboard.type('p12-password');
    await p.keyboard.press('Enter');
    await toast(p, 'Imported the certificate of Alex Morgan');
    await personal().filter({ hasText: 'Harbor Trust Email CA' }).waitFor({ timeout: 5000 });
    await p.keyboard.press('Escape');
    await settings.waitFor({ state: 'detached', timeout: 5000 });
    // A reply to Lukas's S/MIME message, from the personal address, uses S/MIME.
    await openIn(p, 'Angebot für das neue Projekt');
    await p.locator('body').press('r');
    const composer = p.locator('.reader .composer.inline');
    await composer.waitFor({ timeout: 5000 });
    await composer.locator('.icon-btn.lock').click();
    const panel = p.locator('.popover .panel');
    await panel.getByRole('group', { name: 'Encryption standard' }).waitFor({ timeout: 5000 });
    if ((await panel.getByRole('button', { name: 'S/MIME', exact: true }).getAttribute('aria-pressed')) !== 'true') throw new Error('S/MIME is not chosen for the reply');
    await panel.locator('.who', { hasText: 'lukas.weber@kraftwerk-studio.de' }).filter({ hasText: 'has a certificate' }).waitFor({ timeout: 5000 });
    // OpenPGP can still be chosen for this message.
    await panel.getByRole('button', { name: 'OpenPGP', exact: true }).click();
    if ((await panel.getByRole('button', { name: 'OpenPGP', exact: true }).getAttribute('aria-pressed')) !== 'true') throw new Error('OpenPGP could not be chosen');
    await p.keyboard.press('Escape');
  } finally {
    await p.close();
  }
});

/** Picks an entry of the list's View menu. */
async function view(p, name) {
  await p.locator('.list header button[aria-label="View"]').click();
  await p.getByRole('menuitemradio', { name, exact: true }).or(p.getByRole('menuitemcheckbox', { name, exact: true })).click();
}

await step('the reading pane goes below the list, or away, from the View menu, the palette and Settings', async () => {
  const p = await fresh();
  try {
    const list = p.getByRole('listbox', { name: 'Conversations' });
    // Below: the list on top across the window, the conversation under it, with a divider between.
    await view(p, 'Bottom');
    await p.locator('main.below').waitFor({ timeout: 5000 });
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    await p.locator('.reader .subject-block h2').waitFor({ timeout: 5000 });
    const [top, under] = await Promise.all([p.locator('.list').boundingBox(), p.locator('.reader').boundingBox()]);
    if (!(top.y + top.height <= under.y + 1) || top.width < 1000) throw new Error(`the list is at ${JSON.stringify(top)} and the conversation at ${JSON.stringify(under)}`);
    const divider = p.locator('.resizer.across');
    const d = await divider.boundingBox();
    await p.mouse.move(d.x + d.width / 2, d.y + d.height / 2);
    await p.mouse.down();
    await p.mouse.move(d.x + d.width / 2, d.y + 120, { steps: 4 });
    await p.mouse.up();
    const taller = await p.locator('.list').boundingBox();
    if (taller.height < top.height + 100) throw new Error(`dragging the divider left the list ${taller.height} px tall (was ${top.height})`);
    // Off: the open conversation takes the list's place, with Back; the list fills the window.
    await p.keyboard.press('Control+k');
    await p.keyboard.type('reading pane off');
    await p.locator('.palette .row .label', { hasText: /^Reading pane off$/ }).click();
    await p.locator('.reader .back').waitFor({ timeout: 5000 });
    if (await list.count()) throw new Error('the list stays beside the open conversation');
    await p.keyboard.press('Escape');
    await list.waitFor({ timeout: 5000 });
    if (await p.locator('.reader').count()) throw new Error('the conversation stays beside the list');
    if ((await p.locator('.list').boundingBox()).width < 1100) throw new Error('the list does not fill the window');
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    await p.locator('.reader .back').click();
    await list.waitFor({ timeout: 5000 });
    // Settings says so, and puts it back on the right.
    await p.keyboard.press('Control+,');
    await p.locator('.window .tab', { hasText: 'Appearance' }).click();
    const pane = p.getByRole('radiogroup', { name: 'Reading pane' });
    if ((await pane.getByRole('radio', { checked: true }).innerText()).trim() !== 'Off') throw new Error('Settings does not say the pane is off');
    await pane.getByRole('radio', { name: 'Right' }).click();
    await p.keyboard.press('Escape');
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    await p.locator('.reader .subject-block h2').waitFor({ timeout: 5000 });
    if (!(await list.count())) throw new Error('the list did not come back beside the conversation');
  } finally {
    await p.close();
  }
});

await step('without conversations, the list has a row per message and actions take single messages', async () => {
  const p = await fresh();
  try {
    await view(p, 'Group into conversations');
    const rows = p.getByRole('listbox', { name: 'Messages' }).locator('.row', { hasText: 'Q4 roadmap' });
    // The conversation's messages in the inbox: Priya's two and Daniel's (the user's own reply is in Sent).
    await p.locator('.list .search input').fill('Q4 roadmap in:inbox');
    await eventually(p, async () => (await rows.count()) === 3, async () => `${await rows.count()} rows for the conversation's messages`);
    await rows.first().click();
    await p.locator('.reader .of-conversation', { hasText: 'One of 4 messages in this conversation' }).waitFor({ timeout: 5000 });
    if ((await p.locator('.reader .msg').count()) !== 1) throw new Error('the reader shows more than the message');
    await p.locator('body').press('e');
    await toast(p, 'Archived');
    await eventually(p, async () => (await rows.count()) === 2, async () => `${await rows.count()} rows after archiving one message`);
    // Grouped again, the conversation is still in the inbox (its other messages are).
    await view(p, 'Group into conversations');
    await p.locator('.list .search .clear').click();
    await p.getByRole('listbox', { name: 'Conversations' }).locator('.row', { hasText: 'Q4 roadmap' }).first().waitFor({ timeout: 5000 });
  } finally {
    await p.close();
  }
});

await step('a list sorts oldest first or by sender, and each folder keeps its order', async () => {
  const p = await fresh();
  try {
    await p.locator('.tabs button', { hasText: 'All' }).click();
    const senders = () => p.locator('.list .row .from').allInnerTexts();
    const first = () => p.locator('.list .row').first().innerText();
    const newest = await first();
    await view(p, 'Oldest first');
    await eventually(p, async () => (await first()) !== newest, 'the order did not change');
    // Another folder has an order of its own; the inbox keeps oldest first.
    await p.locator('.sidebar .item', { hasText: 'Sent' }).first().click();
    await p.locator('.list h1', { hasText: 'Sent' }).waitFor({ timeout: 5000 });
    await p.locator('.list header button[aria-label="View"]').click();
    if (!(await p.getByRole('menuitemradio', { name: 'Newest first', checked: true }).count())) throw new Error('Sent took the inbox’s order');
    await p.keyboard.press('Escape');
    await p.locator('body').press('g');
    await p.locator('body').press('i');
    await p.locator('.tabs button', { hasText: 'All' }).click();
    await eventually(p, async () => (await first()) !== newest, 'the inbox lost its order');
    // By sender (messages one by one, a sender to a row): alphabetically, whatever the case or accents.
    await view(p, 'Group into conversations');
    await view(p, 'By sender');
    await eventually(p, async () => {
      const names = (await senders()).map((n) => n.toLowerCase().normalize('NFD').replace(/[̀-ͯ]/g, ''));
      return names.length > 3 && names.every((n, i) => i === 0 || names[i - 1] <= n);
    }, async () => `senders in the order ${(await senders()).join(', ')}`);
  } finally {
    await p.close();
  }
});

/** A window of its own opened by `action` (a page of its own in the demo), its errors reported. */
async function popup(p, action) {
  const [win] = await Promise.all([p.context().waitForEvent('page'), action()]);
  win.on('pageerror', (e) => problems.push(`window: ${e.message}`));
  await win.waitForLoadState();
  return win;
}

/** Waits for `win` to close as `action` (a key that closes it as it is pressed, perhaps) is done. */
async function closedBy(win, action) {
  await Promise.all([win.waitForEvent('close', { timeout: 5000 }), action().catch(() => {})]);
}

await step('a conversation opens in a window of its own: Shift+Enter, a double click, the toolbar, the palette', async () => {
  const p = await fresh();
  try {
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    await p.locator('.reader .subject-block h2').waitFor({ timeout: 5000 });
    const win = await popup(p, () => p.keyboard.press('Shift+Enter'));
    if (!/[?&]window=thread&id=\d+/.test(win.url())) throw new Error(`it opened ${win.url()}`);
    await win.locator('.reader .subject-block h2', { hasText: 'Q4 roadmap' }).waitFor({ timeout: 5000 });
    if (await win.locator('.list, .sidebar').count()) throw new Error('the window shows more than the conversation');
    await eventually(win, async () => (await win.title()) === 'Q4 roadmap — final review', async () => `the window is called "${await win.title()}"`);
    // Its keys act on the conversation; Escape closes it.
    await win.locator('body').press('s');
    await win.locator('.reader header .icon-btn.star').waitFor({ timeout: 5000 });
    await closedBy(win, () => win.keyboard.press('Escape'));
    // A double click on a row, the reader's button and the palette open one too.
    const two = await popup(p, () => p.locator('.row', { hasText: 'Offsite planning' }).first().dblclick());
    await two.locator('.reader .subject-block h2', { hasText: 'Offsite planning' }).waitFor({ timeout: 5000 });
    await two.close();
    const three = await popup(p, () => p.locator('.reader header button[aria-label="Open in a new window (Shift+Enter)"]').click());
    await three.locator('.reader .subject-block h2', { hasText: 'Offsite planning' }).waitFor({ timeout: 5000 });
    await three.close();
    await p.keyboard.press('Control+k');
    await p.keyboard.type('new window');
    const four = await popup(p, () => p.locator('.palette .row .label', { hasText: /^Open in a new window$/ }).click());
    await four.locator('.reader .subject-block h2', { hasText: 'Offsite planning' }).waitFor({ timeout: 5000 });
    await four.close();
    // Without conversations, a message opens its conversation in one.
    await view(p, 'Group into conversations');
    await p.locator('.list .search input').fill('Q4 roadmap in:inbox');
    await p.locator('.row', { hasText: 'Q4 roadmap' }).first().click();
    const five = await popup(p, () => p.locator('.reader .of-conversation button', { hasText: 'Open the conversation' }).click());
    await five.locator('.reader .subject-block .chip', { hasText: '4 messages' }).waitFor({ timeout: 5000 });
    await five.close();
  } finally {
    await p.close();
  }
});

await step('a new message pops out into a window of its own, and Escape puts it away', async () => {
  const p = await fresh();
  try {
    await p.locator('body').press('c');
    await p.locator('.dock .composer .subject input').fill('Picnic');
    const win = await popup(p, () => p.locator('.dock .composer .icon-btn[aria-label="Pop out"]').click());
    await win.locator('.composer.windowed .subject input').waitFor({ timeout: 5000 });
    if ((await win.locator('.composer.windowed .subject input').inputValue()) !== 'Picnic') throw new Error('the subject was lost');
    if (await p.locator('.dock .composer').count()) throw new Error('the composer stayed in the main window');
    await eventually(win, async () => (await win.title()) === 'Picnic', async () => `the window is called "${await win.title()}"`);
    // A new message with nothing in it opens empty, and closes with Escape.
    await p.locator('body').press('c');
    const empty = await popup(p, () => p.locator('.dock .composer .icon-btn[aria-label="Pop out"]').click());
    await empty.locator('.composer.windowed .ProseMirror').click();
    await closedBy(empty, () => empty.keyboard.press('Escape'));
    await win.close();
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
