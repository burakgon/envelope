import en from './locales/en.json';
import { format, formatParts, type Params, type Slot } from './message';

/** Every message id. English (`locales/en.json`) is the source of truth. */
export type MessageKey = keyof typeof en;
export type Catalog = Partial<Record<MessageKey, string>>;

/** Languages with a catalog, each named in its own language. */
export const languages = [
  { code: 'en', name: 'English' },
  { code: 'de', name: 'Deutsch' },
  { code: 'es', name: 'Español' },
  { code: 'fr', name: 'Français' },
  { code: 'it', name: 'Italiano' },
  { code: 'pl', name: 'Polski' },
  { code: 'pt-BR', name: 'Português (Brasil)' },
  { code: 'tr', name: 'Türkçe' },
  { code: 'ja', name: '日本語' },
  { code: 'zh-CN', name: '简体中文' },
] as const;

export type Locale = (typeof languages)[number]['code'];
/** The `language` setting: follow the system, or a fixed language. */
export type LanguageSetting = 'system' | Locale;

const english: Catalog = en;

// Non-English catalogs are separate chunks, loaded on demand.
const loaders = import.meta.glob<Catalog>(['./locales/*.json', '!./locales/en.json'], { import: 'default' });
const loaded = new Map<Locale, Catalog>([['en', english]]);

class I18n {
  /** Language of the active catalog. */
  lang = $state<Locale>('en');
  /** BCP 47 tag for dates and numbers; keeps the system's region when it fits ("en-GB", "de-AT"). */
  tag = $state('en');
  catalog = $state.raw<Catalog>(english);
}

export const i18n = new I18n();

function systemLanguages(): readonly string[] {
  if (typeof navigator === 'undefined') return [];
  return navigator.languages?.length ? navigator.languages : [navigator.language].filter(Boolean);
}

/** The catalog for a BCP 47 tag, if there is one ("de-AT" → "de", "pt-PT" → "pt-BR"). */
export function matchLanguage(tag: string): Locale | null {
  let loc: Intl.Locale;
  try {
    loc = new Intl.Locale(tag);
  } catch {
    return null;
  }
  const base = loc.language;
  if (base === 'zh') {
    // Only Simplified Chinese has a catalog.
    const script = loc.script ?? (['TW', 'HK', 'MO'].includes(loc.region ?? '') ? 'Hant' : 'Hans');
    return script === 'Hans' ? 'zh-CN' : null;
  }
  if (base === 'pt') return 'pt-BR';
  return languages.find((l) => l.code === base)?.code ?? null;
}

/** The catalog and formatting locale for a `language` setting. */
export function resolveLanguage(setting: LanguageSetting, preferred: readonly string[] = systemLanguages()): { lang: Locale; tag: string } {
  const lang: Locale = setting !== 'system' ? setting : (preferred.map(matchLanguage).find((l) => l != null) ?? 'en');
  // Keep the user's region for dates and numbers when it matches the language.
  const regional = preferred.find((p) => matchLanguage(p) === lang);
  if (regional) return { lang, tag: regional };
  if (lang === 'en') {
    // English with the conventions of the user's country (24-hour clock, day-month order…).
    try {
      const region = preferred[0] ? new Intl.Locale(preferred[0]).region : undefined;
      if (region) return { lang, tag: `en-${region}` };
    } catch {
      /* not a valid tag */
    }
  }
  return { lang, tag: lang };
}

async function load(lang: Locale): Promise<Catalog> {
  const have = loaded.get(lang);
  if (have) return have;
  const loader = loaders[`./locales/${lang}.json`];
  if (!loader) return english;
  const catalog = await loader();
  loaded.set(lang, catalog);
  return catalog;
}

let request = 0;

/** Switches the interface language, loading its catalog first. Missing messages fall back to English. */
export async function setLanguage(setting: LanguageSetting): Promise<void> {
  const { lang, tag } = resolveLanguage(setting);
  const mine = ++request;
  let catalog: Catalog;
  try {
    catalog = await load(lang);
  } catch (e) {
    console.warn(`Could not load the ${lang} catalog`, e);
    catalog = english;
  }
  if (mine !== request) return;
  i18n.lang = catalog === english ? 'en' : lang;
  i18n.tag = catalog === english && lang !== 'en' ? 'en' : tag;
  i18n.catalog = catalog;
  if (typeof document !== 'undefined') document.documentElement.lang = i18n.tag;
  try {
    localStorage.setItem('envelope.language', setting);
  } catch {
    /* storage unavailable */
  }
}

/** The language setting remembered from the last run, so the first paint is already translated. */
export function storedLanguage(): LanguageSetting {
  try {
    const v = localStorage.getItem('envelope.language');
    if (v === 'system' || languages.some((l) => l.code === v)) return v as LanguageSetting;
  } catch {
    /* storage unavailable */
  }
  return 'system';
}

function lookup(key: MessageKey): { message: string; lang: string } {
  const own = i18n.catalog[key];
  if (own != null) return { message: own, lang: i18n.lang };
  return { message: english[key] ?? key, lang: 'en' };
}

/**
 * Translates a message. Reactive: components and `$derived` values that call it
 * update when the language changes.
 *
 *   t('common.cancel')
 *   t('list.selected', { count: 3 })        // "{count, plural, one {# selected} other {# selected}}"
 *   t('toast.movedTo', { folder: 'Work' })  // "Moved to {folder}"
 */
export function t(key: MessageKey, params?: Params): string {
  const { message, lang } = lookup(key);
  return params || message.includes('{') ? format(message, params ?? {}, lang, i18n.tag) : message;
}

/**
 * Translates a message whose placeholders are rendered by the caller (links, `<kbd>`, bold names).
 * Placeholders given in `params` are filled in; the others come back as `{ slot: name }`.
 *
 *   {#each tParts('reader.hint', { count }) as p}{#if typeof p === 'string'}{p}{:else}<kbd>{key}</kbd>{/if}{/each}
 */
export function tParts(key: MessageKey, params: Params = {}): (string | Slot)[] {
  const { message, lang } = lookup(key);
  return formatParts(message, params, lang, i18n.tag);
}

/**
 * The messages whose keys start with `prefix`, as written (placeholders unfilled), in the
 * interface language with English for gaps. For texts shown outside the web view, such as
 * the `native.*` notification and tray texts the desktop shell fills in itself.
 */
export function rawMessages(prefix: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const key of Object.keys(en) as MessageKey[]) {
    if (key.startsWith(prefix)) out[key] = lookup(key).message;
  }
  return out;
}

/** The BCP 47 locale for `Intl` and `toLocale…String` calls; follows the interface language. */
export function intlLocale(): string {
  return i18n.tag;
}
