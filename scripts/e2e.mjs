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

await step('no page errors', async () => {
  if (problems.length) throw new Error(problems.join('; '));
});

await browser.close();
server.close();
if (failed) {
  console.log(`\n${failed} step(s) failed`);
  process.exit(1);
}
