<script lang="ts">
  import { untrack } from 'svelte';
  import { api } from '../api';
  import type { DraftAttachment } from '../api';
  import { compose, type ComposerState } from '../stores/compose.svelte';
  import { mail } from '../stores/mail.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { dateFormat, displayName, fileSize } from '../utils/format';
  import { t } from '../i18n';
  import RecipientInput from './RecipientInput.svelte';
  import Editor from './Editor.svelte';
  import Popover from './Popover.svelte';
  import AiComposeMenu from './AiComposeMenu.svelte';
  import TemplateMenu from './TemplateMenu.svelte';
  import ComposerCrypto from './ComposerCrypto.svelte';
  import { sanitizeFragment } from '../utils/html';
  import type { Template } from '../stores/ui.svelte';
  import type { Editor as TipTap } from '@tiptap/core';
  import { ChevronDown, Clock, FileText, Maximize, Minimize, Minus, More, Paperclip, Send, Sparkles, Trash, Type, X, File as FileIcon } from '../icons';

  let { state: c }: { state: ComposerState } = $props();

  // Split quoted history (reply/forward) from the editable part.
  function splitQuote(html: string): [string, string] {
    const markers = ['<div class="envelope-quote">', '<div class="envelope-forward">'];
    for (const m of markers) {
      const i = html.indexOf(m);
      if (i >= 0) return [html.slice(0, i), html.slice(i)];
    }
    return [html, ''];
  }

  const initial = untrack(() => splitQuote(c.data.html));
  let body = $state(initial[0]);
  let quoted = $state(initial[1]);
  let showQuote = $state(false);
  let showFormat = $state(false);
  let sendMenu = $state<{ x: number; y: number } | null>(null);
  let aiMenu = $state<{ x: number; y: number } | null>(null);
  let dragging = $state(false);
  let editorRef = $state<TipTap | null>(null);
  let tplMenu = $state<{ x: number; y: number } | null>(null);

  function insertTemplate(tpl: Template) {
    tplMenu = null;
    if (tpl.subject && !c.data.subject.trim()) c.data.subject = tpl.subject;
    if (editorRef) editorRef.chain().focus().insertContent(tpl.html).run();
    else {
      body = tpl.html + body;
      editorKey++;
    }
    changed();
  }
  let customAt = $state('');

  const identities = $derived(mail.identities);
  const identity = $derived(
    identities.find((i) => i.id === c.data.identityId) ?? identities.find((i) => i.accountId === c.data.accountId && i.isDefault),
  );
  const title = $derived(
    c.data.mode === 'forward' ? t('composer.titleForward') : c.data.mode === 'reply' || c.data.mode === 'replyAll' ? t('composer.titleReply') : c.data.subject || t('composer.titleNewMessage'),
  );

  function changed() {
    c.data.html = body + quoted;
    compose.changed(c.key);
  }

  function onBody(html: string) {
    body = html;
    changed();
  }

  async function addPaths(paths: string[]) {
    if (!paths.length) return;
    try {
      const infos = await api.fileInfo(paths);
      c.data.attachments = [
        ...c.data.attachments,
        ...infos.map<DraftAttachment>((f) => ({ filename: f.filename, mimeType: f.mimeType, size: f.size, path: f.path })),
      ];
      changed();
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function addFiles(files: File[]) {
    for (const f of files) {
      if (f.size > 35 * 1024 * 1024) {
        toasts.error(t('composer.fileTooLarge', { name: f.name }));
        continue;
      }
      const bytes = new Uint8Array(await f.arrayBuffer());
      const info = await api.stashFile(f.name, bytes);
      c.data.attachments = [...c.data.attachments, { filename: f.name, mimeType: f.type || info.mimeType, size: f.size, path: info.path }];
    }
    changed();
  }

  async function attach() {
    addPaths(await api.pickFiles());
  }

  function removeAttachment(i: number) {
    c.data.attachments = c.data.attachments.filter((_, j) => j !== i);
    changed();
  }

  function onDrop(e: DragEvent) {
    dragging = false;
    const files = [...(e.dataTransfer?.files ?? [])];
    if (!files.length) return;
    e.preventDefault();
    addFiles(files);
  }

  let cryptoBar: ReturnType<typeof ComposerCrypto> | undefined = $state();

  async function send(at: number | null = null) {
    c.data.html = body + quoted;
    sendMenu = null;
    // Encryption: every recipient needs a key, a locked key its passphrase.
    if (cryptoBar && !(await cryptoBar.prepareSend())) return;
    compose.send(c.key, at);
  }

  function laterOptions() {
    const d = (days: number, h: number) => {
      const x = new Date();
      x.setDate(x.getDate() + days);
      x.setHours(h, 0, 0, 0);
      return x;
    };
    const nextMonday = d(((8 - new Date().getDay()) % 7) || 7, 8);
    return [
      { label: t('composer.tomorrowMorning'), when: d(1, 8) },
      { label: t('composer.tomorrowAfternoon'), when: d(1, 13) },
      { label: t('composer.mondayMorning'), when: nextMonday },
    ];
  }

  function onKey(e: KeyboardEvent) {
    // In the body the editor sends (and says so by preventing the default).
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter' && !e.defaultPrevented) {
      e.preventDefault();
      send();
    } else if (e.key === 'Escape' && !c.inline) {
      e.stopPropagation();
      compose.close(c.key);
    }
  }

  function applyAi(text: string, replace: boolean) {
    const html = text
      .split(/\n{2,}/)
      .map((p) => `<p>${p.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/\n/g, '<br>')}</p>`)
      .join('');
    // Keep the signature below generated text.
    const sigIndex = body.indexOf('envelope-signature');
    if (replace || !body.replace(/<[^>]*>|\s/g, '')) body = html;
    else body = html + body;
    if (sigIndex >= 0 && replace) {
      const sig = mail.identities.find((i) => i.id === c.data.identityId)?.signature;
      if (sig) body += `<div class="envelope-signature">${sig}</div>`;
    }
    c.focus++;
    changed();
    editorKey++;
  }
  let editorKey = $state(0);
</script>

<div
  class="composer"
  class:inline={c.inline}
  class:floating={!c.inline}
  class:min={c.minimized}
  class:max={c.maximized}
  class:dragging
  role="dialog"
  aria-label={title}
  tabindex="-1"
  onkeydown={onKey}
  ondragover={(e) => { if (e.dataTransfer?.types.includes('Files')) { e.preventDefault(); dragging = true; } }}
  ondragleave={() => (dragging = false)}
  ondrop={onDrop}
>
  {#if !c.inline}
    <header class="bar" role="presentation" ondblclick={() => (c.maximized = !c.maximized)}>
      <button class="title truncate" onclick={() => (c.minimized = !c.minimized)}>{title}</button>
      {#if c.saving}<span class="saved">{t('common.saving')}</span>{:else if c.savedAt}<span class="saved">{t('common.saved')}</span>{/if}
      <button class="icon-btn sm" title={t('composer.minimize')} onclick={() => (c.minimized = !c.minimized)}><Minus size={15} /></button>
      <button class="icon-btn sm" title={c.maximized ? t('composer.restore') : t('composer.maximize')} onclick={() => { c.maximized = !c.maximized; c.minimized = false; }}>
        {#if c.maximized}<Minimize size={14} />{:else}<Maximize size={14} />{/if}
      </button>
      <button class="icon-btn sm" title={t('composer.closeShortcut', { shortcut: t('keys.esc') })} onclick={() => compose.close(c.key)}><X size={15} /></button>
    </header>
  {/if}

  {#if !c.minimized}
    <div class="fields">
      {#if identities.length > 1}
        <div class="from">
          <span class="lbl">{t('composer.from')}</span>
          <select
            value={identity?.id}
            onchange={(e) => {
              const id = Number((e.currentTarget as HTMLSelectElement).value);
              const i = identities.find((x) => x.id === id);
              if (i) {
                c.data.identityId = i.id;
                c.data.accountId = i.accountId;
                changed();
              }
            }}
          >
            {#each identities as i (i.id)}
              <option value={i.id}>{i.name} &lt;{i.email}&gt;</option>
            {/each}
          </select>
        </div>
      {/if}
      <RecipientInput label={t('composer.to')} bind:value={c.data.to} onchange={changed} autofocus={c.data.to.length === 0 && c.data.mode !== 'reply'}>
        <span class="ccbcc">
          {#if !c.showCc}<button onclick={() => (c.showCc = true)}>{t('composer.cc')}</button>{/if}
          {#if !c.showBcc}<button onclick={() => (c.showBcc = true)}>{t('composer.bcc')}</button>{/if}
        </span>
      </RecipientInput>
      {#if c.showCc}<RecipientInput label={t('composer.cc')} bind:value={c.data.cc} onchange={changed} />{/if}
      {#if c.showBcc}<RecipientInput label={t('composer.bcc')} bind:value={c.data.bcc} onchange={changed} />{/if}
      {#if !c.inline || c.data.mode === 'forward'}
        <div class="subject">
          <input placeholder={t('composer.subject')} bind:value={c.data.subject} oninput={changed} spellcheck="true" />
        </div>
      {/if}
    </div>

    <div class="body">
      {#key editorKey}
        <Editor
          html={body}
          onupdate={onBody}
          focus={c.focus}
          showToolbar={showFormat}
          onkeysend={() => send()}
          onpastefiles={addFiles}
          bind:editorRef
          placeholder={c.data.mode === 'new' ? t('composer.messagePlaceholder') : t('composer.replyPlaceholder')}
        />
      {/key}
      {#if quoted}
        <button class="quote-toggle" title={t('composer.showQuotedText')} onclick={() => (showQuote = !showQuote)}>•••</button>
        {#if showQuote}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <div
            class="quoted"
            onclick={(e) => {
              const a = (e.target as HTMLElement).closest('a[href]');
              if (!a) return;
              e.preventDefault();
              const href = a.getAttribute('href') ?? '';
              if (/^(https?:|mailto:)/i.test(href)) api.openUrl(href);
            }}
          >{@html sanitizeFragment(quoted)}</div>
        {/if}
      {/if}
      {#if c.data.attachments.length}
        <div class="atts">
          {#each c.data.attachments as a, i (a.filename + i)}
            <span class="att">
              <FileIcon size={14} />
              <span class="truncate">{a.filename}</span>
              <span class="sz">{fileSize(a.size)}</span>
              <button aria-label={t('composer.removeAttachment')} onclick={() => removeAttachment(i)}><X size={12} /></button>
            </span>
          {/each}
        </div>
      {/if}
    </div>

    <footer>
      <div class="send">
        <button class="btn primary send-main" onclick={() => send()} title={t('composer.sendShortcut', { shortcut: `${t('keys.ctrl')}+${t('keys.enter')}` })}>
          <Send size={15} /> {t('common.send')}
        </button>
        <button class="btn primary send-more" aria-label={t('composer.sendLater')} onclick={(e) => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); sendMenu = { x: r.left, y: r.top }; }}>
          <ChevronDown size={14} />
        </button>
      </div>
      <button class="icon-btn" title={t('composer.attachFiles')} onclick={attach}><Paperclip size={17} /></button>
      <button class="icon-btn" class:active={showFormat} title={t('composer.formatting')} onclick={() => (showFormat = !showFormat)}><Type size={17} /></button>
      <button class="icon-btn" title={t('composer.templates')} onclick={(e) => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); tplMenu = { x: r.left, y: r.top - 8 }; }}><FileText size={17} /></button>
      <button class="icon-btn ai" title={t('composer.writeWithAi')} onclick={(e) => { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); aiMenu = { x: r.left, y: r.top }; }}><Sparkles size={17} /></button>
      <ComposerCrypto state={c} onchange={changed} bind:this={cryptoBar} />

      <span class="grow"></span>
      {#if c.inline}
        {#if c.saving}<span class="saved">{t('common.saving')}</span>{:else if c.savedAt}<span class="saved">{t('compose.draftSaved')}</span>{/if}
        <button class="icon-btn" title={t('composer.popOut')} onclick={() => { c.inline = false; }}><More size={17} /></button>
      {/if}
      <button class="icon-btn" title={t('composer.discardDraft')} aria-label={t('composer.discardDraft')} onclick={() => compose.discard(c.key)}><Trash size={17} /></button>
    </footer>
  {/if}

  {#if dragging}
    <div class="drop"><Paperclip size={22} /> {t('composer.dropFiles')}</div>
  {/if}
</div>

{#if sendMenu}
  <Popover x={sendMenu.x} y={sendMenu.y - 8} onclose={() => (sendMenu = null)} width={320}>
    <div class="menu-label">{t('composer.scheduleSend')}</div>
    {#each laterOptions() as o (o.label)}
      <button class="menu-item" onclick={() => send(o.when.getTime())}>
        <Clock size={15} />{o.label}
        <span class="hint">{dateFormat({ weekday: 'short', hour: 'numeric', minute: '2-digit' }).format(o.when)}</span>
      </button>
    {/each}
    <div class="divider"></div>
    <div class="custom">
      <input class="input" type="datetime-local" bind:value={customAt} />
      <button class="btn sm primary" disabled={!customAt} onclick={() => send(new Date(customAt).getTime())}>{t('composer.schedule')}</button>
    </div>
    <div class="divider"></div>
    <div class="menu-label">{t('composer.remindIfNoReply')}</div>
    <div class="followup">
      {#each [[null, t('common.off')], [1, t('composer.followUpDays', { count: 1 })], [3, t('composer.followUpDays', { count: 3 })], [7, t('composer.followUpWeeks', { count: 1 })]] as [days, label] (label)}
        <button
          class:on={(c.data.followUpDays ?? null) === days}
          onclick={() => { c.data.followUpDays = days as number | null; changed(); }}>{label}</button>
      {/each}
    </div>
    <div class="divider"></div>
    <label class="menu-item receipt">
      <input type="checkbox" checked={!!c.data.requestReadReceipt} onchange={(e) => { c.data.requestReadReceipt = (e.currentTarget as HTMLInputElement).checked; changed(); }} />
      {t('composer.requestReadReceipt')}
    </label>
  </Popover>
{/if}

{#if tplMenu}
  <TemplateMenu x={tplMenu.x} y={tplMenu.y} subject={c.data.subject} {body} onclose={() => (tplMenu = null)} oninsert={insertTemplate} />
{/if}

{#if aiMenu}
  <AiComposeMenu
    x={aiMenu.x}
    y={aiMenu.y}
    draft={c.data}
    currentText={body}
    onclose={() => (aiMenu = null)}
    onapply={(text, replace) => { aiMenu = null; applyAi(text, replace); }}
    onsubject={(s) => { if (!c.data.subject) { c.data.subject = s; changed(); } }}
  />
{/if}

<style>
  .composer {
    position: relative;
    display: flex;
    flex-direction: column;
    background: var(--bg-elevated);
  }
  .composer.inline {
    border-radius: 14px;
    box-shadow: 0 0 0 1px var(--border-strong), var(--shadow-md);
    padding: 6px 16px 10px;
    animation: slide-up var(--dur) var(--ease-out);
  }
  .composer.floating {
    width: 580px;
    height: min(600px, calc(100vh - 80px));
    border-radius: 14px 14px 0 0;
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    pointer-events: auto;
    animation: slide-up var(--dur-slow) var(--ease-out);
  }
  .composer.floating.min {
    height: 42px;
    width: 300px;
  }
  .composer.floating.max {
    position: fixed;
    inset: 6vh 8vw;
    width: auto;
    height: auto;
    border-radius: 16px;
    z-index: 800;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 2px;
    height: 42px;
    padding: 0 6px 0 14px;
    background: var(--bg-sunken);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .title {
    flex: 1;
    text-align: left;
    font-weight: 600;
  }
  .saved {
    font-size: 11.5px;
    color: var(--text-3);
    margin-right: 6px;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fields {
    padding: 0 16px;
    flex-shrink: 0;
  }
  .inline .fields {
    padding: 0;
  }
  .from {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 38px;
    border-bottom: 1px solid var(--border);
  }
  .from .lbl {
    min-width: 54px;
    color: var(--text-3);
  }
  .from select {
    flex: 1;
    border: none;
    font-weight: 500;
    height: 30px;
    appearance: none;
    -webkit-appearance: none;
    padding-right: 24px;
    background: transparent url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%238b8f9c' stroke-width='2.4' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='m6 9 6 6 6-6'/%3E%3C/svg%3E") no-repeat right 4px center / 13px;
  }
  .ccbcc {
    display: flex;
    gap: 2px;
  }
  .ccbcc button {
    font-size: var(--text-sm);
    color: var(--text-3);
    padding: 2px 6px;
    border-radius: 6px;
  }
  .ccbcc button:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .subject {
    border-bottom: 1px solid var(--border);
  }
  .subject input {
    width: 100%;
    height: 40px;
    border: none;
    background: none;
    font-weight: 600;
    font-size: var(--text-md);
  }
  .body {
    flex: 1;
    overflow-y: auto;
    padding: 10px 16px 6px;
  }
  .inline .body {
    padding: 10px 0 4px;
    overflow: visible;
  }
  .quote-toggle {
    padding: 0 9px;
    height: 18px;
    line-height: 16px;
    border-radius: 9px;
    font-size: 13px;
    letter-spacing: 1px;
    color: var(--text-3);
    background: var(--bg-active);
  }
  .followup {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 2px 8px 6px;
  }
  .followup button {
    flex: 1;
    height: 28px;
    padding: 0 6px;
    white-space: nowrap;
    border-radius: 7px;
    font-size: var(--text-sm);
    font-weight: 500;
    color: var(--text-2);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .followup button:hover {
    background: var(--bg-hover);
  }
  .followup button.on {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
  }
  .receipt input {
    accent-color: var(--accent);
    width: 15px;
    height: 15px;
  }
  .quoted {
    margin-top: 8px;
    padding-left: 12px;
    border-left: 2px solid var(--border-strong);
    color: var(--text-2);
    max-height: 340px;
    overflow: auto;
    font-size: var(--text-base);
    user-select: text;
  }
  .quoted :global(img) {
    max-width: 100%;
  }
  .atts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 10px;
  }
  .att {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 260px;
    height: 30px;
    padding: 0 6px 0 10px;
    border-radius: 8px;
    font-size: var(--text-sm);
    font-weight: 500;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .att .sz {
    color: var(--text-3);
    flex-shrink: 0;
  }
  .att button {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 5px;
    color: var(--text-3);
  }
  .att button:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  footer {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 8px 12px 12px;
    flex-shrink: 0;
  }
  .inline footer {
    padding: 8px 0 2px;
  }
  .send {
    display: flex;
    margin-right: 6px;
  }
  .send-main {
    border-radius: 9px 0 0 9px;
    height: 32px;
    padding: 0 14px;
  }
  .send-more {
    border-radius: 0 9px 9px 0;
    height: 32px;
    padding: 0 7px;
    margin-left: 1px;
  }
  .ai {
    color: var(--accent-text);
  }
  .grow {
    flex: 1;
  }
  .custom {
    display: flex;
    gap: 6px;
    padding: 4px;
  }
  .custom .input {
    height: 28px;
    font-size: var(--text-sm);
  }
  .drop {
    position: absolute;
    inset: 8px;
    border-radius: 12px;
    border: 2px dashed var(--accent);
    background: color-mix(in oklab, var(--bg-elevated) 85%, var(--accent));
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 8px;
    font-weight: 600;
    color: var(--accent-text);
    pointer-events: none;
    z-index: 5;
  }
</style>
