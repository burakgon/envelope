<script lang="ts">
  import { ui, type MailRule, type RuleAction, type RuleCondition } from '../../stores/ui.svelte';
  import { mail, roleLabel } from '../../stores/mail.svelte';
  import Toggle from './Toggle.svelte';
  import { ArrowDown, ArrowUp, Play, Plus, X } from '../../icons';
  import { runRule, runsOnExisting } from '../../stores/rules.svelte';
  import { intlLocale, t, tParts } from '../../i18n';

  let editing = $state<MailRule | null>(ui.rulesDraft);
  /** Run the rule on the mail already here once it is saved. */
  let applyExisting = $state(false);
  $effect(() => {
    // "Create rule…" from a message opens the editor prefilled.
    if (ui.rulesDraft) {
      editing = ui.rulesDraft;
      ui.rulesDraft = null;
      applyExisting = false;
    }
  });

  const fields: { value: RuleCondition['field']; label: string }[] = $derived([
    { value: 'from', label: t('settings.rules.fieldFrom') },
    { value: 'to', label: t('settings.rules.fieldTo') },
    { value: 'subject', label: t('settings.rules.fieldSubject') },
    { value: 'any', label: t('settings.rules.fieldAny') },
  ]);
  const ops: { value: RuleCondition['op']; label: string }[] = $derived([
    { value: 'contains', label: t('settings.rules.opContains') },
    { value: 'notContains', label: t('settings.rules.opNotContains') },
    { value: 'is', label: t('settings.rules.opIs') },
    { value: 'endsWith', label: t('settings.rules.opEndsWith') },
  ]);
  const actionKinds: { value: RuleAction['kind']; label: string }[] = $derived([
    { value: 'move', label: t('settings.rules.actionMove') },
    { value: 'label', label: t('settings.rules.actionLabel') },
    { value: 'archive', label: t('settings.rules.actionArchive') },
    { value: 'read', label: t('settings.rules.actionRead') },
    { value: 'star', label: t('settings.rules.actionStar') },
    { value: 'mute', label: t('settings.rules.actionMute') },
    { value: 'spam', label: t('settings.rules.actionSpam') },
    { value: 'trash', label: t('common.delete') },
  ]);

  const scopeFolders = $derived(
    mail.folders.filter((f) => editing && (editing.accountId == null || f.accountId === editing.accountId)),
  );
  const folderName = (id: number) => {
    const f = mail.folders.find((x) => x.id === id);
    if (!f) return t('settings.rules.missingFolder');
    const name = f.role ? roleLabel(f.role) : f.path;
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

  // Lists joined the way the interface language does it: "a and b", "a, b, or c".
  const andList = $derived(new Intl.ListFormat(intlLocale(), { type: 'conjunction' }));
  const orList = $derived(new Intl.ListFormat(intlLocale(), { type: 'disjunction' }));

  function summary(r: MailRule): string {
    const when = r.conditions
      .filter((c) => c.value.trim())
      .map((c) => t('settings.rules.summaryCondition', { field: c.field, op: c.op, value: c.value }));
    const then = andList.format(
      r.actions.map((a) => t('settings.rules.summaryAction', { kind: a.kind, folder: 'folderId' in a ? folderName(a.folderId) : null })),
    );
    if (!when.length) return t('settings.rules.summaryAlways', { actions: then });
    return t('settings.rules.summary', { conditions: (r.match === 'all' ? andList : orList).format(when), actions: then });
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
    if (applyExisting && runsOnExisting(rule)) runRule(rule);
  }

  function edit(rule: MailRule) {
    editing = rule;
    applyExisting = false;
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

<h2>{t('settings.tab.rules')}</h2>
<p class="desc">{t('settings.rules.desc')}</p>

<div class="accounts">
  {#each ui.settings.rules as r, i (r.id)}
    <div class="acct rule" class:off={!r.enabled}>
      <div class="acct-info">
        <b>{r.name}</b>
        <span class="acct-sub">{summary(r)}</span>
      </div>
      <div class="order">
        <button class="icon-btn sm" aria-label={t('settings.rules.moveUp')} disabled={i === 0} onclick={() => moveRule(i, -1)}><ArrowUp size={14} /></button>
        <button class="icon-btn sm" aria-label={t('settings.rules.moveDown')} disabled={i === ui.settings.rules.length - 1} onclick={() => moveRule(i, 1)}><ArrowDown size={14} /></button>
      </div>
      <Toggle checked={r.enabled} label={t('settings.rules.enabled')} onchange={(v) => toggle(r, v)} />
      <button class="btn sm" disabled={!runsOnExisting(r)} title={t('rules.runNowTitle')} onclick={() => runRule($state.snapshot(r) as MailRule)}><Play size={12} />{t('rules.runNow')}</button>
      <button class="btn sm" onclick={() => edit(structuredClone($state.snapshot(r)))}>{t('common.edit')}</button>
    </div>
  {/each}
</div>
<button class="btn" onclick={() => edit(blank())}><Plus size={15} />{t('settings.rules.new')}</button>

{#if editing}
  <div class="editor-card">
    <div class="two">
      <div><label class="label" for="rule-name">{t('settings.rules.name')}</label><input id="rule-name" class="input" placeholder={t('settings.rules.namePlaceholder')} bind:value={editing.name} /></div>
      <div>
        <label class="label" for="rule-acct">{t('settings.rules.account')}</label>
        <select id="rule-acct" class="input" value={String(editing.accountId ?? '')} onchange={(e) => (editing!.accountId = Number((e.currentTarget as HTMLSelectElement).value) || null)}>
          <option value="">{t('settings.rules.allAccounts')}</option>
          {#each mail.accounts as a (a.id)}<option value={String(a.id)}>{a.label ?? a.email}</option>{/each}
        </select>
      </div>
    </div>

    <div class="sect">
      <div class="sect-head">
        {#each tParts('settings.rules.matchSentence') as part}
          {#if typeof part === 'string'}{part}{:else}<select class="input mini" aria-label={t('settings.rules.match')} bind:value={editing.match}><option value="all">{t('settings.rules.matchAll')}</option><option value="any">{t('settings.rules.matchAny')}</option></select>{/if}
        {/each}
      </div>
      {#each editing.conditions as c, i (i)}
        <div class="line">
          <select class="input" aria-label={t('settings.rules.field')} bind:value={c.field}>{#each fields as f (f.value)}<option value={f.value}>{f.label}</option>{/each}</select>
          <select class="input" aria-label={t('settings.rules.comparison')} bind:value={c.op}>{#each ops as o (o.value)}<option value={o.value}>{o.label}</option>{/each}</select>
          <input class="input grow" placeholder={c.field === 'subject' ? t('settings.rules.subjectPlaceholder') : t('settings.rules.addressPlaceholder')} aria-label={t('settings.rules.value')} bind:value={c.value} />
          <button class="icon-btn sm" aria-label={t('settings.rules.removeCondition')} disabled={editing.conditions.length === 1} onclick={() => editing!.conditions.splice(i, 1)}><X size={14} /></button>
        </div>
      {/each}
      <button class="add" onclick={() => editing!.conditions.push({ field: 'subject', op: 'contains', value: '' })}><Plus size={14} />{t('settings.rules.addCondition')}</button>
    </div>

    <div class="sect">
      <div class="sect-head">{t('settings.rules.then')}</div>
      {#each editing.actions as a, i (i)}
        <div class="line">
          <select class="input act" aria-label={t('settings.rules.action')} value={a.kind} onchange={(e) => setAction(i, (e.currentTarget as HTMLSelectElement).value as RuleAction['kind'])}>
            {#each actionKinds as k (k.value)}<option value={k.value}>{k.label}</option>{/each}
          </select>
          {#if a.kind === 'move' || a.kind === 'label'}
            {@const options = scopeFolders.filter((f) => (a.kind === 'label' ? f.kind === 'label' : f.selectable && f.kind !== 'label'))}
            <select class="input grow" aria-label={a.kind === 'label' ? t('settings.rules.labelTarget') : t('mailbox.folder')} value={String(a.folderId)} onchange={(e) => ('folderId' in a) && (a.folderId = Number((e.currentTarget as HTMLSelectElement).value))}>
              {#each options as f (f.id)}<option value={String(f.id)}>{folderName(f.id)}</option>{:else}<option value="0">{a.kind === 'label' ? t('settings.rules.noLabels') : t('settings.rules.noFolders')}</option>{/each}
            </select>
          {:else}
            <span class="grow"></span>
          {/if}
          <button class="icon-btn sm" aria-label={t('settings.rules.removeAction')} disabled={editing.actions.length === 1} onclick={() => editing!.actions.splice(i, 1)}><X size={14} /></button>
        </div>
      {/each}
      <button class="add" onclick={() => editing!.actions.push({ kind: 'read' })}><Plus size={14} />{t('settings.rules.addAction')}</button>
    </div>

    <label class="stop"><Toggle checked={editing.stop} onchange={(v) => (editing!.stop = v)} /> {t('settings.rules.stop')}</label>
    {#if runsOnExisting(editing)}
      <label class="stop"><Toggle checked={applyExisting} label={t('rules.applyExisting')} onchange={(v) => (applyExisting = v)} /> {t('rules.applyExisting')}</label>
    {/if}

    <div class="btns">
      {#if editing.id}<button class="btn danger" onclick={remove}>{t('common.delete')}</button>{/if}
      <span class="grow"></span>
      <button class="btn" onclick={() => (editing = null)}>{t('common.cancel')}</button>
      <button class="btn primary" disabled={!valid} onclick={save}>{t('settings.rules.save')}</button>
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
    flex-wrap: wrap;
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
  /* Fits the longest option (translations run longer), but leaves room for the value. */
  .line > select:not(.grow) {
    width: auto;
    min-width: 190px;
    max-width: 40%;
    flex-shrink: 0;
  }
  /* An action has at most a folder next to it, so it can take more room. */
  .line > select.act {
    max-width: 60%;
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
