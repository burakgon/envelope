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

// ----- attachment reminder --------------------------------------------------------------------

const { mentionedAttachment, ownText, wordList, sendable, EMAIL } = await import(new URL('utils/compose.ts', src).href);

const catalogs = new Map();
function words(lang) {
  if (!catalogs.has(lang)) catalogs.set(lang, JSON.parse(readFileSync(new URL(`i18n/locales/${lang}.json`, src), 'utf8')));
  return wordList(catalogs.get(lang)['compose.attachmentWords'], catalogs.get('en')?.['compose.attachmentWords'] ?? '');
}
words('en');
for (const [lang, html, want] of [
  ['en', '<p>Please find the report attached.</p>', 'attached'],
  ['en', '<p>The attachment is the signed copy.</p>', 'attachment'],
  ['en', '<p>Unattached parts are fine.</p>', null],
  ['en', '<p>Thanks!</p><div class="envelope-quote"><div>On Mon, Alex wrote:</div><blockquote>I attached the file</blockquote></div>', null],
  ['en', '<p>Thanks!</p><blockquote><p>see attached</p></blockquote>', null],
  ['en', '<p>Hi</p><div class="envelope-signature"><p>Attachments are scanned for viruses</p></div>', null],
  ['de', '<p>Anbei die Rechnung.</p>', 'anbei'],
  ['de', '<p>Im Anhang findest du den Plan.</p>', 'anhang'],
  ['es', '<p>Te envío el contrato adjunto.</p>', 'adjunto'],
  ['fr', '<p>Voici la facture en pièce jointe.</p>', 'pièce jointe'],
  ['fr', '<p>Voir le document ci-joint.</p>', 'ci-joint'],
  ['it', '<p>In allegato trovi il preventivo.</p>', 'in allegato'],
  ['pl', '<p>W załączniku przesyłam umowę.</p>', 'załączniku'],
  ['pt-BR', '<p>Segue o arquivo em anexo.</p>', 'em anexo'],
  ['tr', '<p>Fatura ekte.</p>', 'ekte'],
  ['tr', '<p>Ekmek aldım.</p>', null],
  ['ja', '<p>資料を添付しました。</p>', '添付'],
  ['zh-CN', '<p>请查收附件。</p>', '附件'],
  // English is always understood, whatever the interface language.
  ['de', '<p>See the attached plan.</p>', 'attached'],
]) {
  check(`[${lang}] ${ownText(html).trim()}`, mentionedAttachment(html, words(lang)), want);
}
check('quoted lines of a plain-text reply are not the writer\'s', ownText('<p>ok</p><p>&gt; attached</p>').includes('attached'), false);

// ----- addresses ------------------------------------------------------------------------------

for (const [email, valid, canSend] of [
  ['maya@northwind.design', true, true],
  ['bob@example', false, true],
  ['bob', false, false],
  ['bob @example.com', false, false],
]) {
  check(`${email} looks complete`, EMAIL.test(email), valid);
  check(`${email} can be sent to`, sendable(email), canSend);
}

// ----- links ------------------------------------------------------------------------------------

const { linkTarget, namedHost, shorten } = await import(new URL('utils/links.ts', src).href);

const target = (href, text) => {
  const l = linkTarget(href, text);
  return l && { kind: l.kind, host: l.host, claimed: l.claimed };
};
check('an ordinary link', target('https://dailybyte.test/read', 'Read the full issue →'), { kind: 'web', host: 'dailybyte.test', claimed: null });
check('text naming another site', target('https://paypal.com.secure-login.test/x', 'https://www.paypal.com/signin'), { kind: 'web', host: 'paypal.com.secure-login.test', claimed: 'paypal.com' });
check('a bare domain as text', target('http://evil.example/', 'paypal.com'), { kind: 'web', host: 'evil.example', claimed: 'paypal.com' });
check('the same site, other host', target('https://click.mail.example.com/t?u=1', 'example.com'), { kind: 'web', host: 'click.mail.example.com', claimed: null });
check('www does not matter', target('https://example.com/', 'www.example.com'), { kind: 'web', host: 'example.com', claimed: null });
check('a file name is not a site', namedHost('report.pdf'), null);
check('a sentence is not a site', namedHost('Download report.pdf now'), null);
check('mail links', target('mailto:maya@northwind.design?subject=Hi', 'Write to Maya'), { kind: 'mail', host: null, claimed: null });
check('links inside the message', linkTarget('#top', 'Top'), null);
check('long addresses are shortened in the middle', shorten('https://example.com/' + 'a'.repeat(100), 40).length, 40);

// ----- search queries -------------------------------------------------------------------

const { buildQuery, addressAtEnd, completeAddress, noOptions } = await import(new URL('utils/searchquery.ts', src).href);

const query = (o) => buildQuery({ ...noOptions, ...o });
check('no options, no query', query({}), '');
check('options in the engine\'s words', query({ from: 'maya', attachment: true, within: '1m', words: 'budget' }), 'from:maya has:attachment newer_than:1m budget');
check('a value with spaces is quoted', query({ from: ' Maya Chen ', subject: 'Q3 "plan"' }), 'from:"Maya Chen" subject:"Q3 plan"');
check('every option', query({ to: 'sam', unread: true, starred: true, vip: true, folder: 'inbox' }), 'to:sam is:unread is:starred is:vip in:inbox');
check('a folder path with spaces', query({ folder: '[Gmail]/Sent Mail' }), 'in:"[Gmail]/Sent Mail"');
check('an address being typed', addressAtEnd('budget from:ma'), { key: 'from', text: 'ma' });
check('a negated one, just begun', addressAtEnd('-to:'), { key: 'to', text: '' });
check('in any case', addressAtEnd('CC:sam'), { key: 'cc', text: 'sam' });
check('an address already typed is left alone', addressAtEnd('from:maya budget'), null);
check('nor is a word that only ends like one', addressAtEnd('photo:x'), null);
check('completing an address', completeAddress('budget from:ma', 'maya@northwind.design'), 'budget from:maya@northwind.design ');
check('completing keeps the key as typed', completeAddress('TO:s', 'sam@envelope.test'), 'TO:sam@envelope.test ');

if (failed) {
  console.log(`\n${failed} of ${checks} check(s) failed`);
  process.exit(1);
}
console.log(`ok   ${checks} checks of reading and writing mail`);
