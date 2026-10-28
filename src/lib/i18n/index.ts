// Interface translations. See `state.svelte.ts` for the API and `locales/en.json`
// for the source messages; `npm run i18n:check` keeps the other catalogs in step.

export {
  i18n,
  intlLocale,
  languages,
  matchLanguage,
  rawMessages,
  resolveLanguage,
  setLanguage,
  storedLanguage,
  t,
  tParts,
  type Catalog,
  type LanguageSetting,
  type Locale,
  type MessageKey,
} from './state.svelte';
export type { Params, Slot } from './message';
