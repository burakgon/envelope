<script lang="ts">
  import { ui, type Template } from '../stores/ui.svelte';
  import { toasts } from '../stores/toasts.svelte';
  import { htmlToText } from '../utils/html';
  import Popover from './Popover.svelte';
  import { FileText, Plus, Settings } from '../icons';

  let {
    x,
    y,
    subject,
    body,
    onclose,
    oninsert,
  }: {
    x: number;
    y: number;
    /** The draft's current subject and body, for "save as template". */
    subject: string;
    body: string;
    onclose: () => void;
    oninsert: (t: Template) => void;
  } = $props();

  let q = $state('');
  let naming = $state(false);
  let name = $state('');

  const list = $derived(
    ui.settings.templates.filter((t) => !q.trim() || `${t.name} ${t.subject}`.toLowerCase().includes(q.trim().toLowerCase())),
  );

  function save() {
    const n = name.trim();
    if (!n) return;
    const sig = body.indexOf('<div class="envelope-signature">');
    const html = sig >= 0 ? body.slice(0, sig) : body;
    const t: Template = { id: crypto.randomUUID(), name: n, subject, html };
    ui.set('templates', [...ui.settings.templates, t]);
    toasts.show(`Saved template “${n}”`, { kind: 'success' });
    onclose();
  }
</script>

<Popover {x} {y} {onclose} width={300}>
  {#if naming}
    <div class="naming">
      <div class="menu-label">Save as template</div>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="input"
        placeholder="Template name"
        bind:value={name}
        autofocus
        onkeydown={(e) => {
          if (e.key === 'Enter') save();
          if (e.key === 'Escape') {
            e.stopPropagation();
            naming = false;
          }
        }}
      />
      <div class="btns">
        <button class="btn sm" onclick={() => (naming = false)}>Cancel</button>
        <button class="btn sm primary" disabled={!name.trim()} onclick={save}>Save</button>
      </div>
    </div>
  {:else}
    {#if ui.settings.templates.length > 6}
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input find" placeholder="Find a template" bind:value={q} autofocus />
    {/if}
    {#if ui.settings.templates.length}
      <div class="menu-label">Insert template</div>
      <div class="list">
        {#each list as t (t.id)}
          <button class="menu-item tpl" onclick={() => oninsert(t)}>
            <FileText size={15} />
            <span class="txt">
              <b class="truncate">{t.name}</b>
              <span class="truncate">{t.subject || htmlToText(t.html).slice(0, 80)}</span>
            </span>
          </button>
        {:else}
          <div class="none">No matching templates</div>
        {/each}
      </div>
      <div class="divider"></div>
    {:else}
      <div class="empty">Templates are messages you write once and reuse — save this draft to create your first one.</div>
    {/if}
    <button class="menu-item" disabled={!htmlToText(body).trim()} onclick={() => (naming = true)}><Plus size={15} />Save draft as template</button>
    <button class="menu-item" onclick={() => { onclose(); ui.open('settings', 'templates'); }}><Settings size={15} />Manage templates</button>
  {/if}
</Popover>

<style>
  .find {
    margin: 4px 4px 6px;
    width: calc(100% - 8px);
  }
  .list {
    max-height: 280px;
    overflow-y: auto;
  }
  .tpl {
    height: auto;
    padding-top: 6px;
    padding-bottom: 6px;
    align-items: flex-start;
  }
  .tpl :global(svg) {
    margin-top: 2px;
  }
  .txt {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    text-align: left;
  }
  .txt span {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .none,
  .empty {
    padding: 8px 10px;
    font-size: var(--text-sm);
    color: var(--text-3);
    line-height: 1.45;
  }
  .naming {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 4px;
  }
  .btns {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
</style>
