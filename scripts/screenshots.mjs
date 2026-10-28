// Captures README screenshots from the in-browser demo.
// Usage: npm run demo (in another terminal), then: node scripts/screenshots.mjs [outDir]
import { chromium } from 'playwright';
import { mkdirSync } from 'node:fs';

const out = process.argv[2] ?? 'docs/screenshots';
const url = process.env.DEMO_URL ?? 'http://127.0.0.1:1420/';
mkdirSync(out, { recursive: true });

const browser = await chromium.launch({
  executablePath: process.env.CHROMIUM ?? '/usr/bin/chromium',
  args: ['--font-render-hinting=none'],
});

async function shot(name, { theme = 'light', width = 1440, height = 900, actions = async () => {} } = {}) {
  const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 2, colorScheme: theme });
  await page.addInitScript((t) => {
    localStorage.setItem('envelope.theme', t);
    localStorage.setItem('envelope.demo.settings', JSON.stringify({ theme: t }));
  }, theme);
  await page.goto(url);
  await page.waitForSelector('.row', { timeout: 15000 });
  await page.waitForTimeout(400);
  await actions(page);
  if (!name.includes('compose') && !name.includes('palette')) await page.mouse.move(width - 5, height - 5);
  await page.waitForTimeout(500);
  await page.screenshot({ path: `${out}/${name}.png` });
  await page.close();
  console.log('saved', name);
}

const openFirst = async (page) => {
  await page.locator('.row').first().click();
  await page.waitForTimeout(700);
};

const only = process.argv[3];
const jobs = {
  'inbox-light': () => shot('inbox-light', { actions: openFirst }),
  'inbox-dark': () => shot('inbox-dark', { theme: 'dark', actions: openFirst }),
  thread: () => shot('thread', { actions: async (p) => { await p.locator('.row', { hasText: 'Q4 roadmap' }).click(); await p.waitForTimeout(600); } }),
  compose: () => shot('compose', { actions: async (p) => { await openFirst(p); await p.keyboard.press('c'); await p.waitForTimeout(300); await p.keyboard.type('pri'); await p.waitForTimeout(400); } }),
  palette: () => shot('palette', { theme: 'dark', actions: async (p) => { await openFirst(p); await p.keyboard.press('Control+k'); await p.keyboard.type('arch'); } }),
  settings: () => shot('settings', { actions: async (p) => { await p.keyboard.press('Control+,'); await p.waitForTimeout(200); await p.locator('.tab', { hasText: 'Appearance' }).click(); } }),
};
for (const [name, job] of Object.entries(jobs)) {
  if (!only || only === name) await job();
}
await browser.close();
