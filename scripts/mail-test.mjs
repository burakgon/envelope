#!/usr/bin/env node
// Tests the parts of reading and writing mail that need no window, such as plain-text mail as
// HTML and the line before a quote folded away with it. Runs the interface's own modules in
// Node, which strips their types; the i18n module (Svelte state) is replaced by a stand-in.
//
//   npm run test:mail

import { registerHooks } from 'node:module';
import { readFileSync } from 'node:fs';

const i18n = `data:text/javascript,${encodeURIComponent('export const t = (key) => key; export const intlLocale = () => "en";')}`;
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === '../i18n') return { url: i18n, shortCircuit: true };
    // The interface imports its modules without the `.ts`.
    if (specifier.startsWith('.') && !/\.[cm]?[jt]s$/.test(specifier)) {
      try {
        return next(`${specifier}.ts`, context);
      } catch {
        /* not a TypeScript module */
      }
    }
    return next(specifier, context);
  },
});

const src = new URL('../src/lib/', import.meta.url);

let failed = 0;
let checks = 0;
function check(name, got, want) {
  checks++;
  const g = JSON.stringify(got);
  const w = JSON.stringify(want);
  if (g === w) return;
  failed++;
  console.log(`FAIL ${name}\n     got  ${g}\n     want ${w}`);
}

// ----- plain-text mail ------------------------------------------------------------------

const { textToHtml, isAttribution, attributionOnly, blockLines } = await import(new URL('utils/html.ts', src).href);

/** Text of HTML, as shown. */
const text = (html) => html.replace(/<[^>]+>/g, '').replace(/&quot;/g, '"').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&');

/** The blocks textToHtml makes, as [kind, text]. */
function blocks(html) {
  return [...html.matchAll(/<div class="env-plain( env-attribution)?">([\s\S]*?)<\/div>|<blockquote type="cite">([\s\S]*?)<\/blockquote>/g)].map((m) =>
    m[3] != null ? ['quote', text(m[3])] : [m[1] ? 'intro' : 'text', text(m[2])],
  );
}

// The demo's "Re: Book recommendations": the reply must not fold away with the quote (1.2).
check(
  'a short reply stays apart from the line before the quote',
  blocks(textToHtml('Ha, I knew you\'d like it. Next up: "The Pragmatic Programmer", then we argue about it over dinner.\n\nOn Mon, Alex Morgan wrote:\n> Finished "A Philosophy of Software Design" — what next?')),
  [
    ['text', 'Ha, I knew you\'d like it. Next up: "The Pragmatic Programmer", then we argue about it over dinner.'],
    ['intro', 'On Mon, Alex Morgan wrote:'],
    ['quote', 'Finished "A Philosophy of Software Design" — what next?'],
  ],
);
check(
  'without a blank line before it, too',
  blocks(textToHtml('Sounds good!\nOn Thu, Oct 29, 2026 at 2:32 PM Elif <elif@example.com> wrote:\n> Saturday?')),
  [['text', 'Sounds good!'], ['intro', 'On Thu, Oct 29, 2026 at 2:32 PM Elif <elif@example.com> wrote:'], ['quote', 'Saturday?']],
);
check(
  'an introduction wrapped onto two lines goes as one',
  blocks(textToHtml('See you.\n\nOn Thu, Oct 29, 2026 at 2:32 PM Elif Yılmaz <elif@example.com>\nwrote:\n> Saturday?')),
  [['text', 'See you.'], ['intro', 'On Thu, Oct 29, 2026 at 2:32 PM Elif Yılmaz <elif@example.com>\nwrote:'], ['quote', 'Saturday?']],
);
check(
  'a short introduction under a reply line is not taken for a wrapped one',
  blocks(textToHtml('Call me at 5\nOn Mon, Alex wrote:\n> Free today?')),
  [['text', 'Call me at 5'], ['intro', 'On Mon, Alex wrote:'], ['quote', 'Free today?']],
);
check('paragraphs at blank lines', blocks(textToHtml('Hi,\n\nfirst\nsecond\n\n\nBye')), [['text', 'Hi,'], ['text', 'first\nsecond'], ['text', 'Bye']]);
check('a reply that ends in a colon is not an introduction', blocks(textToHtml('Here is the list:\n> one')), [['text', 'Here is the list:'], ['quote', 'one']]);
check('links', textToHtml('See https://example.com/a.'), '<div class="env-plain">See <a href="https://example.com/a">https://example.com/a</a>.</div>');
for (const [line, want] of [
  ['On Mon, Alex Morgan wrote:', true],
  ['Am 29.10.2026 um 14:32 schrieb Elif <elif@example.com>:', true],
  ['Le jeu. 29 oct. 2026 à 14:32, Elif a écrit :', true],
  ['El jue, 29 oct 2026 a las 14:32, Elif escribió:', true],
  ['Elif <elif@example.com>, 29 Eki 2026 Per, 14:32 tarihinde şunu yazdı:', true],
  ['2026年10月29日(木) 14:32 Elif さんは書きました:', true],
  ['Elif 于 2026年10月29日 写道：', true],
  ['Here is what she wrote', false],
  ['Agenda:', false],
]) {
  check(`introduction: ${line}`, isAttribution(line), want);
}
// What collapseQuotes folds away with a quote: a block that is only the introduction.
check('a block that is only the introduction', attributionOnly(blockLines('On Mon, Alex wrote:<br>')), true);
check('a reply that ends with the introduction', attributionOnly(blockLines('Thanks!<br>On Mon, Alex wrote:')), false);
check('Gmail\'s wrapped introduction', attributionOnly(blockLines('On Mon, Oct 26, 2026 at 9:00 AM Alex Morgan &lt;alex@lumen.dev&gt;<br>wrote:')), true);

if (failed) {
  console.log(`\n${failed} of ${checks} check(s) failed`);
  process.exit(1);
}
console.log(`ok   ${checks} checks of reading and writing mail`);
