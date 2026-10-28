// A tiny subset of ICU MessageFormat:
//
//   {name}                                    interpolation
//   {count, plural, =0 {…} one {# item} other {# items}}
//   {kind, select, mail {…} event {…} other {…}}
//
// `#` inside a plural branch is the (locale-formatted) number. Branches may nest
// placeholders and other plurals/selects. There is no quoting: `{` always opens a
// placeholder, so messages never contain literal braces.

export type Params = Record<string, string | number | null | undefined>;

/** A placeholder left unfilled by `tParts`, for the caller to render (e.g. as a link or `<kbd>`). */
export interface Slot {
  slot: string;
}

type Part = string | Hash | Arg | Choice;
interface Hash {
  hash: true;
}
interface Arg {
  name: string;
}
interface Choice {
  name: string;
  type: 'plural' | 'select';
  options: Record<string, Part[]>;
}

const HASH: Hash = { hash: true };

export function parse(src: string): Part[] {
  let i = 0;
  const fail = (what: string): never => {
    throw new SyntaxError(`${what} at ${i} in "${src}"`);
  };
  const space = () => {
    while (i < src.length && /\s/.test(src[i])) i++;
  };
  const word = () => {
    const start = i;
    while (i < src.length && !/[\s,{}]/.test(src[i])) i++;
    return src.slice(start, i);
  };

  function body(hash: boolean, nested: boolean): Part[] {
    const out: Part[] = [];
    let text = '';
    const flush = () => {
      if (text) out.push(text);
      text = '';
    };
    while (i < src.length) {
      const c = src[i];
      if (c === '{') {
        flush();
        i++;
        out.push(placeholder(hash));
      } else if (c === '}' && nested) {
        break;
      } else if (c === '#' && hash) {
        flush();
        out.push(HASH);
        i++;
      } else {
        text += c;
        i++;
      }
    }
    flush();
    return out;
  }

  function placeholder(hash: boolean): Part {
    space();
    const name = word();
    if (!name) fail('Expected a placeholder name');
    space();
    if (src[i] === '}') {
      i++;
      return { name };
    }
    if (src[i] !== ',') fail('Expected "," or "}"');
    i++;
    space();
    const type = word();
    if (type !== 'plural' && type !== 'select') fail(`Unknown placeholder type "${type}"`);
    space();
    if (src[i] !== ',') fail('Expected ","');
    i++;
    const options: Record<string, Part[]> = {};
    for (;;) {
      space();
      if (i >= src.length) fail('Unclosed placeholder');
      if (src[i] === '}') {
        i++;
        break;
      }
      const key = word();
      if (!key) fail('Expected a branch name');
      space();
      if (src[i] !== '{') fail('Expected "{"');
      i++;
      options[key] = body(hash || type === 'plural', true);
      if (src[i] !== '}') fail('Unclosed branch');
      i++;
    }
    if (!options.other) fail('Missing "other" branch');
    return { name, type: type as Choice['type'], options };
  }

  const parts = body(false, false);
  return parts;
}

const cache = new Map<string, Part[]>();
const pluralRules = new Map<string, Intl.PluralRules>();
const numberFormats = new Map<string, Intl.NumberFormat>();

function rules(lang: string): Intl.PluralRules {
  let r = pluralRules.get(lang);
  if (!r) pluralRules.set(lang, (r = new Intl.PluralRules(lang)));
  return r;
}

export function formatNumber(n: number, locale: string): string {
  let f = numberFormats.get(locale);
  if (!f) numberFormats.set(locale, (f = new Intl.NumberFormat(locale)));
  return f.format(n);
}

function render(parts: Part[], params: Params, lang: string, locale: string, num: number | null, slots: boolean, out: (string | Slot)[]) {
  for (const p of parts) {
    if (typeof p === 'string') out.push(p);
    else if ('hash' in p) out.push(num == null ? '#' : formatNumber(num, locale));
    else if (!('type' in p)) {
      const v = params[p.name];
      if (v != null) out.push(String(v));
      else if (slots) out.push({ slot: p.name });
      else out.push(`{${p.name}}`);
    } else if (p.type === 'plural') {
      const n = Number(params[p.name] ?? 0);
      const branch = p.options[`=${n}`] ?? p.options[rules(lang).select(n)] ?? p.options.other;
      render(branch, params, lang, locale, n, slots, out);
    } else {
      const branch = p.options[String(params[p.name])] ?? p.options.other;
      render(branch, params, lang, locale, num, slots, out);
    }
  }
}

function compiled(message: string): Part[] {
  let parts = cache.get(message);
  if (!parts) {
    try {
      parts = parse(message);
    } catch (e) {
      console.warn(e);
      parts = [message];
    }
    cache.set(message, parts);
  }
  return parts;
}

/**
 * Fills a message. `lang` picks the plural rules (the language the message is
 * written in), `locale` formats numbers (it may carry a region).
 */
export function format(message: string, params: Params, lang: string, locale: string): string {
  if (!message.includes('{')) return message;
  const out: (string | Slot)[] = [];
  render(compiled(message), params, lang, locale, null, false, out);
  return out.join('');
}

/** Like `format`, but placeholders without a value come back as slots. Adjacent text is merged. */
export function formatParts(message: string, params: Params, lang: string, locale: string): (string | Slot)[] {
  const raw: (string | Slot)[] = [];
  render(compiled(message), params, lang, locale, null, true, raw);
  const out: (string | Slot)[] = [];
  for (const p of raw) {
    const last = out[out.length - 1];
    if (typeof p === 'string' && typeof last === 'string') out[out.length - 1] = last + p;
    else out.push(p);
  }
  return out;
}
