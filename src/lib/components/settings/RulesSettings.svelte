<script lang="ts">
  import { ui, type MailRule, type RuleAction, type RuleCondition } from '../../stores/ui.svelte';
  import { mail, roleLabels } from '../../stores/mail.svelte';
  import Toggle from './Toggle.svelte';
  import { ArrowDown, ArrowUp, Plus, X } from '../../icons';

  let editing = $state<MailRule | null>(ui.rulesDraft);
  $effect(() => {
    // "Create rule…" from a message opens the editor prefilled.
    if (ui.rulesDraft) {
      editing = ui.rulesDraft;
      ui.rulesDraft = null;
    }
  });

  const fields: { value: RuleCondition['field']; label: string }[] = [
    { value: 'from', label: 'From' },
    { value: 'to', label: 'To or Cc' },
    { value: 'subject', label: 'Subject' },
    { value: 'any', label: 'From, to or subject' },
  ];
  const ops: { value: RuleCondition['op']; label: string }[] = [
    { value: 'contains', label: 'contains' },
    { value: 'notContains', label: 'does not contain' },
    { value: 'is', label: 'is' },
    { value: 'endsWith', label: 'ends with' },
  ];
  const actionKinds: { value: RuleAction['kind']; label: string }[] = [
    { value: 'move', label: 'Move to folder' },
    { value: 'label', label: 'Apply label / category' },
    { value: 'archive', label: 'Archive (skip the inbox)' },
    { value: 'read', label: 'Mark as read' },
    { value: 'star', label: 'Star' },
    { value: 'mute', label: 'Don’t notify' },
    { value: 'spam', label: 'Mark as spam' },
    { value: 'trash', label: 'Delete' },
  ];

  const scopeFolders = $derived(
    mail.folders.filter((f) => editing && (editing.accountId == null || f.accountId === editing.accountId)),
  );
  const folderName = (id: number) => {
    const f = mail.folders.find((x) => x.id === id);
    if (!f) return 'Missing folder';
    const name = f.role ? roleLabels[f.role] : f.path;
    const a = mail.multiAccount && !editing?.accountId ? mail.accountById.get(f.accountId) : null;
    return a ? `${name} — ${a.label ?? a.email}` : name;
  };

  function blank(): MailRule {
    return {
      id: '',
      name: '',
      enabled: true,
      accountId: null,
      match: 'all',
      conditions: [{ field: 'from', op: 'contains', value: '' }],
      actions: [{ kind: 'archive' }],
      stop: false,
    };
  }

  function setAction(i: number, kind: RuleAction['kind']) {
    if (!editing) return;
    const firstFolder = (labels: boolean) => scopeFolders.find((f) => (labels ? f.kind === 'label' : f.selectable && f.kind !== 'label'))?.id ?? 0;
    editing.actions[i] = kind === 'move' ? { kind, folderId: firstFolder(false) } : kind === 'label' ? { kind, folderId: firstFolder(true) } : { kind };
  }

  function summary(r: MailRule): string {
    const when = r.conditions
      .filter((c) => c.value.trim())
      .map((c) => `${fields.find((f) => f.value === c.field)?.label.toLowerCase()} ${ops.find((o) => o.value === c.op)?.label} “${c.value}”`)
      .join(r.match === 'all' ? ' and ' : ' or ');
    const then = r.actions
      .map((a) => ('folderId' in a ? `${a.kind === 'move' ? 'move to' : 'label'} ${folderName(a.folderId)}` : actionKinds.find((k) => k.value === a.kind)?.label.toLowerCase()))
      .join(', ');
    return `If ${when || 'any message'}: ${then}`;
  }

  function save() {
    if (!editing) return;
    const conditions = editing.conditions.filter((c) => c.value.trim());
    if (!conditions.length || !editing.actions.length) return;
    const rule: MailRule = {
      ...$state.snapshot(editing),
      id: editing.id || crypto.randomUUID(),
      name: editing.name.trim() || conditions.map((c) => c.value.trim()).join(', '),
      conditions,
    };
    const list = ui.settings.rules;
    ui.set('rules', list.some((r) => r.id === rule.id) ? list.map((r) => (r.id === rule.id ? rule : r)) : [...list, rule]);
    editing = null;
  }

  function remove() {
    if (!editing) return;
    const id = editing.id;
    ui.set('rules', ui.settings.rules.filter((r) => r.id !== id));
    editing = null;
  }

  function toggle(r: MailRule, enabled: boolean) {
    ui.set('rules', ui.settings.rules.map((x) => (x.id === r.id ? { ...x, enabled } : x)));
  }

  function moveRule(i: number, dir: -1 | 1) {
    const list = [...ui.settings.rules];
    const j = i + dir;
    if (j < 0 || j >= list.length) return;
    [list[i], list[j]] = [list[j], list[i]];
    ui.set('rules', list);
  }

  const valid = $derived(!!editing && editing.conditions.some((c) => c.value.trim()) && editing.actions.length > 0);
</script>

<h2>Rules</h2>
<p class="desc">Rules sort new mail as it arrives — move it, label it, mark it read or keep it quiet. They run in order, top to bottom.</p>

<div class="accounts">
  {#each ui.settings.rules as r, i (r.id)}
    <div class="acct rule" class:off={!r.enabled}>
      <div class="acct-info">
        <b>{r.name}</b>
        <span class="acct-sub">{summary(r)}</span>
      </div>
      <div class="order">
        <button class="icon-btn sm" aria-label="Move up" disabled={i === 0} onclick={() => moveRule(i, -1)}><ArrowUp size={14} /></button>
        <button class="icon-btn sm" aria-label="Move down" disabled={i === ui.settings.rules.length - 1} onclick={() => moveRule(i, 1)}><ArrowDown size={14} /></button>
      </div>
      <Toggle checked={r.enabled} label="Enabled" onchange={(v) => toggle(r, v)} />
      <button class="btn sm" onclick={() => (editing = structuredClone($state.snapshot(r)))}>Edit</button>
    </div>
  {/each}
</div>
<button class="btn" onclick={() => (editing = blank())}><Plus size={15} />New rule</button>

{#if editing}
  <div class="editor-card">
    <div class="two">
      <div><label class="label" for="rule-name">Name</label><input id="rule-name" class="input" placeholder="e.g. Receipts" bind:value={editing.name} /></div>
      <div>
        <label class="label" for="rule-acct">Account</label>
        <select id="rule-acct" class="input" value={String(editing.accountId ?? '')} onchange={(e) => (editing!.accountId = Number((e.currentTarget as HTMLSelectElement).value) || null)}>
          <option value="">All accounts</option>
          {#each mail.accounts as a (a.id)}<option value={String(a.id)}>{a.label ?? a.email}</option>{/each}
        </select>
      </div>
    </div>

    <div class="sect">
      <div class="sect-head">
        When a new message matches
        <select class="input mini" bind:value={editing.match}><option value="all">all</option><option value="any">any</option></select>
        of these
      </div>
      {#each editing.conditions as c, i (i)}
        <div class="line">
          <select class="input" bind:value={c.field}>{#each fields as f (f.value)}<option value={f.value}>{f.label}</option>{/each}</select>
          <select class="input" bind:value={c.op}>{#each ops as o (o.value)}<option value={o.value}>{o.label}</option>{/each}</select>
          <input class="input grow" placeholder={c.field === 'subject' ? 'Invoice' : 'name@example.com'} bind:value={c.value} />
          <button class="icon-btn sm" aria-label="Remove condition" disabled={editing.conditions.length === 1} onclick={() => editing!.conditions.splice(i, 1)}><X size={14} /></button>
        </div>
      {/each}
      <button class="add" onclick={() => editing!.conditions.push({ field: 'subject', op: 'contains', value: '' })}><Plus size={14} />Add condition</button>
    </div>

    <div class="sect">
      <div class="sect-head">Then</div>
      {#each editing.actions as a, i (i)}
        <div class="line">
          <select class="input" value={a.kind} onchange={(e) => setAction(i, (e.currentTarget as HTMLSelectElement).value as RuleAction['kind'])}>
            {#each actionKinds as k (k.value)}<option value={k.value}>{k.label}</option>{/each}
          </select>
          {#if a.kind === 'move' || a.kind === 'label'}
            {@const options = scopeFolders.filter((f) => (a.kind === 'label' ? f.kind === 'label' : f.selectable && f.kind !== 'label'))}
            <select class="input grow" value={String(a.folderId)} onchange={(e) => ('folderId' in a) && (a.folderId = Number((e.currentTarget as HTMLSelectElement).value))}>
              {#each options as f (f.id)}<option value={String(f.id)}>{folderName(f.id)}</option>{:else}<option value="0">{a.kind === 'label' ? 'No labels' : 'No folders'}</option>{/each}
            </select>
          {:else}
            <span class="grow"></span>
          {/if}
          <button class="icon-btn sm" aria-label="Remove action" disabled={editing.actions.length === 1} onclick={() => editing!.actions.splice(i, 1)}><X size={14} /></button>
        </div>
      {/each}
      <button class="add" onclick={() => editing!.actions.push({ kind: 'read' })}><Plus size={14} />Add action</button>
    </div>

    <label class="stop"><Toggle checked={editing.stop} onchange={(v) => (editing!.stop = v)} /> Stop here — don’t run later rules on matching messages</label>

    <div class="btns">
      {#if editing.id}<button class="btn danger" onclick={remove}>Delete</button>{/if}
      <span class="grow"></span>
      <button class="btn" onclick={() => (editing = null)}>Cancel</button>
      <button class="btn primary" disabled={!valid} onclick={save}>Save rule</button>
    </div>
  </div>
{/if}

<style>
  h2 {
    margin: 0 0 16px;
    font-size: var(--text-xl);
    font-weight: 680;
    letter-spacing: -0.02em;
  }
  .desc {
    color: var(--text-2);
    line-height: 1.55;
    margin: 0 0 14px;
  }
  .accounts {
    display: grid;
    gap: 8px;
    margin-bottom: 14px;
  }
  .acct {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .acct-info {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .acct-sub {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .editor-card {
    display: grid;
    gap: 14px;
    padding: 16px;
    margin-top: 14px;
    border-radius: 14px;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .btns {
    display: flex;
    gap: 8px;
  }
  .rule.off {
    opacity: 0.55;
  }
  .rule .acct-sub {
    white-space: normal;
  }
  .order {
    display: flex;
  }
  .sect {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .sect-head {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 600;
    color: var(--text-2);
  }
  .mini {
    width: auto;
    height: 28px;
  }
  .line {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .line > select {
    width: 190px;
    flex-shrink: 0;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .add {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 8px;
    border-radius: 7px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--accent-text);
  }
  .add:hover {
    background: var(--accent-softer);
  }
  .stop {
    display: flex;
    align-items: center;
    gap: 10px;
    color: var(--text-2);
  }
</style>
