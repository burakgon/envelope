<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { AiModel, AiPreset, AiSettings } from '../../api';
  import { ai } from '../../stores/ai.svelte';
  import { ui } from '../../stores/ui.svelte';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import Toggle from './Toggle.svelte';
  import { CheckCircle, ExternalLink, Lock, Refresh, ShieldCheck, Spinner } from '../../icons';
  import { intlLocale, t } from '../../i18n';

  let presets = $state<AiPreset[]>([]);
  let settings = $state<AiSettings>({ enabled: false, provider: 'openrouter', baseUrl: '', model: '', customInstructions: '', language: null });
  let hasKey = $state(false);
  let key = $state('');
  let models = $state<AiModel[]>([]);
  let loadingModels = $state(false);
  let testing = $state(false);
  let status = $state<{ ok: boolean; text: string } | null>(null);
  let filter = $state('');

  const preset = $derived(presets.find((p) => p.id === settings.provider));
  const shownModels = $derived(
    models.filter((m) => !filter || `${m.id} ${m.name}`.toLowerCase().includes(filter.toLowerCase())).slice(0, 200),
  );

  onMount(async () => {
    presets = await api.ai.presets();
    const cfg = await api.ai.config();
    const { hasKey: hk, local: _local, ...rest } = cfg;
    settings = rest;
    hasKey = hk;
    if (settings.enabled && (hasKey || preset?.local)) loadModels();
  });

  function choose(id: string) {
    const p = presets.find((x) => x.id === id);
    settings.provider = id;
    settings.baseUrl = p?.baseUrl ?? '';
    settings.model = p?.defaultModel ?? '';
    models = [];
    status = null;
    hasKey = false;
    key = '';
  }

  async function loadModels() {
    loadingModels = true;
    try {
      models = await api.ai.models($state.snapshot(settings), key || null);
    } catch (e) {
      status = { ok: false, text: errorMessage(e) };
    } finally {
      loadingModels = false;
    }
  }

  async function test() {
    testing = true;
    status = null;
    try {
      status = { ok: true, text: await api.ai.test($state.snapshot(settings), key || null) };
    } catch (e) {
      status = { ok: false, text: errorMessage(e) };
    } finally {
      testing = false;
    }
  }

  async function save() {
    try {
      await api.ai.setConfig($state.snapshot(settings), key || null);
      if (key) hasKey = true;
      key = '';
      await ai.load();
      ui.set('aiEnabled', settings.enabled);
      toasts.show(t('settings.ai.saved'), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  const usd = $derived(
    new Intl.NumberFormat(intlLocale(), { style: 'currency', currency: 'USD', currencyDisplay: 'narrowSymbol', minimumFractionDigits: 2, maximumFractionDigits: 2 }),
  );
  const price = (m: AiModel) =>
    m.inputPrice != null && m.inputPrice >= 0 ? t('settings.ai.price', { input: usd.format(m.inputPrice), output: usd.format(m.outputPrice ?? 0) }) : '';
</script>

<h2>{t('settings.tab.ai')}</h2>
<p class="desc">
  {t('settings.ai.desc')}
</p>

<div class="group">
  <div class="row">
    <div class="txt"><b>{t('settings.ai.enable')}</b><span>{t('settings.ai.enableDesc')}</span></div>
    <Toggle checked={settings.enabled} onchange={(v) => (settings.enabled = v)} />
  </div>
</div>

<h3>{t('settings.ai.provider')}</h3>
<div class="providers">
  {#each presets as p (p.id)}
    <button class="prov" class:on={settings.provider === p.id} onclick={() => choose(p.id)}>
      <span class="pname">{p.name}</span>
      <span class="pkind">{p.local ? t('settings.ai.local') : p.kind}</span>
    </button>
  {/each}
</div>

<div class="form">
  {#if preset?.local || settings.provider === 'custom' || !preset}
    <div>
      <label class="label" for="ai-url">{t('settings.ai.baseUrl')}</label>
      <input id="ai-url" class="input" bind:value={settings.baseUrl} placeholder="http://localhost:11434/v1" spellcheck="false" />
    </div>
  {/if}
  {#if preset?.requiresKey !== false}
    <div>
      <label class="label" for="ai-key">{t('settings.ai.apiKey')} {#if hasKey}<span class="saved"><Lock size={11} /> {t('settings.ai.keySaved')}</span>{/if}</label>
      <div class="inline">
        <input id="ai-key" class="input" type="password" bind:value={key} placeholder={hasKey ? '••••••••••••••••' : t('settings.ai.apiKeyPlaceholder')} spellcheck="false" />
        {#if preset?.keyUrl}
          <button class="btn" onclick={() => api.openUrl(preset!.keyUrl!)}>{t('settings.ai.getKey')} <ExternalLink size={13} /></button>
        {/if}
      </div>
    </div>
  {/if}
  <div>
    <label class="label" for="ai-model">{t('settings.ai.model')}</label>
    <div class="inline">
      <input id="ai-model" class="input" bind:value={settings.model} placeholder={t('settings.ai.modelPlaceholder')} spellcheck="false" list="ai-models" oninput={() => (filter = settings.model)} />
      <button class="btn" onclick={loadModels} disabled={loadingModels}>
        {#if loadingModels}<span class="spin"><Spinner size={14} /></span>{:else}<Refresh size={14} />{/if} {t('settings.ai.loadModels')}
      </button>
    </div>
    {#if models.length}
      <div class="models">
        {#each shownModels as m (m.id)}
          <button class="model" class:on={settings.model === m.id} onclick={() => { settings.model = m.id; filter = ''; }}>
            <span class="mname truncate">{m.name}</span>
            <span class="mid truncate">{m.id}</span>
            <span class="mprice">{price(m)}</span>
          </button>
        {/each}
      </div>
    {/if}
  </div>
  <div>
    <label class="label" for="ai-style">{t('settings.ai.style')}</label>
    <textarea id="ai-style" class="input area" rows="3" bind:value={settings.customInstructions} placeholder={t('settings.ai.stylePlaceholder')}></textarea>
  </div>
  <div>
    <label class="label" for="ai-lang">{t('settings.ai.language')}</label>
    <select id="ai-lang" class="input" bind:value={settings.language}>
      <option value={null}>{t('settings.ai.languageSame')}</option>
      {#each ['English', 'Türkçe', 'Deutsch', 'Español', 'Français', 'Italiano', 'Português', 'Nederlands', 'Polski', '日本語', '中文', '한국어', 'Русский'] as l (l)}
        <option value={l}>{l}</option>
      {/each}
    </select>
  </div>
</div>

{#if status}
  <p class="status" class:ok={status.ok}>{#if status.ok}<CheckCircle size={15} />{/if}{status.text}</p>
{/if}

<div class="btns">
  <button class="btn" onclick={test} disabled={testing}>{#if testing}<span class="spin"><Spinner size={14} /></span>{/if}{t('settings.ai.test')}</button>
  <span class="grow"></span>
  <button class="btn primary" onclick={save}>{t('common.save')}</button>
</div>

<p class="privacy"><ShieldCheck size={14} /> {t('settings.ai.privacy')}</p>

<style>
  h2 {
    margin: 0 0 8px;
    font-size: var(--text-xl);
    font-weight: 680;
  }
  h3 {
    margin: 22px 0 10px;
    font-size: var(--text-md);
  }
  .desc {
    color: var(--text-2);
    line-height: 1.55;
    margin: 0 0 16px;
  }
  .group {
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 16px;
  }
  .txt {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .txt b {
    font-weight: 600;
  }
  .txt span {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .providers {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 8px;
  }
  .prov {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 10px 12px;
    border-radius: 12px;
    text-align: left;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .prov:hover {
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .prov.on {
    background: var(--accent-softer);
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .pname {
    font-weight: 650;
  }
  .pkind {
    font-size: 11.5px;
    color: var(--text-3);
  }
  .form {
    display: grid;
    gap: 14px;
    margin-top: 16px;
  }
  .inline {
    display: flex;
    gap: 8px;
  }
  .saved {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-left: 8px;
    color: var(--green);
    font-weight: 550;
  }
  .models {
    margin-top: 8px;
    max-height: 220px;
    overflow-y: auto;
    border-radius: 10px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
    padding: 4px;
  }
  .model {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 32px;
    padding: 0 10px;
    border-radius: 7px;
    text-align: left;
  }
  .model:hover {
    background: var(--bg-hover);
  }
  .model.on {
    background: var(--accent-soft);
  }
  .mname {
    font-weight: 550;
    max-width: 40%;
  }
  .mid {
    flex: 1;
    color: var(--text-3);
    font-size: 11.5px;
    font-family: var(--font-mono);
  }
  .mprice {
    font-size: 11px;
    color: var(--text-3);
    flex-shrink: 0;
  }
  .area {
    height: auto;
    padding: 8px 11px;
    resize: vertical;
  }
  .status {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--red-soft);
    color: var(--red);
    margin: 14px 0 0;
  }
  .status.ok {
    background: var(--green-soft);
    color: var(--green);
  }
  .btns {
    display: flex;
    gap: 8px;
    margin-top: 16px;
  }
  .grow {
    flex: 1;
  }
  .privacy {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    margin-top: 18px;
    color: var(--text-3);
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .spin {
    display: grid;
    animation: spin 0.9s linear infinite;
  }
</style>
