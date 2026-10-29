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
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
const problems = [];
page.on('pageerror', (e) => problems.push(`page error: ${e.message}`));
page.on('console', (m) => m.type() === 'error' && problems.push(`console error: ${m.text()}`));
await page.addInitScript(() => sessionStorage.setItem('envelope.demo.banner', 'hidden'));

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

await step('no page errors', async () => {
  if (problems.length) throw new Error(problems.join('; '));
});

await browser.close();
server.close();
if (failed) {
  console.log(`\n${failed} step(s) failed`);
  process.exit(1);
}
