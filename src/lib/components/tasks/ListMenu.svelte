<script lang="ts">
  import { onMount, tick } from 'svelte';
  import type { TaskList } from '../../api';
  import { listName, tasks } from '../../stores/tasks.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { t } from '../../i18n';
  import Popover from '../Popover.svelte';
  import { BrushCleaning, Eye, EyeOff, Pen, Trash } from '../../icons';

  type Mode = 'menu' | 'rename' | 'create' | 'delete' | 'clear';

  let {
    list = null,
    accountId = null,
    x,
    y,
    align = 'start',
    start = 'menu',
    showCompletedToggle = false,
    onclose,
  }: {
    /** The list acted on; null when creating one. */
    list?: TaskList | null;
    /** The account a new list goes to (null: this computer). */
    accountId?: number | null;
    x: number;
    y: number;
    align?: 'start' | 'end';
    start?: Mode;
    /** Offer to show or hide completed tasks (in the list's own view). */
    showCompletedToggle?: boolean;
    onclose: () => void;
  } = $props();

  let mode = $state<Mode>('menu');
  let name = $state('');
  let field: HTMLInputElement | undefined = $state();

  onMount(() => go(start));

  const account = $derived.by(() => {
    const id = list ? list.accountId : accountId;
    const a = id != null ? mail.accountById.get(id) : null;
    return a ? (a.label ?? a.email) : null;
  });
  const viewing = $derived(!!list && tasks.isView({ list: list.id }));
  // The local list is named in the interface language, so it keeps its name.
  const renamable = $derived(!!list && !list.readOnly && !(list.kind === 'local' && list.isDefault));
  const deletable = $derived(!!list && !list.readOnly && !list.isDefault);

  async function go(m: Mode) {
    mode = m;
    if (m === 'rename' || m === 'create') {
      name = m === 'rename' && list ? list.name : '';
      await tick();
      field?.focus();
      field?.select();
    }
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const value = name.trim();
    if (!value) return;
    // Props follow the opener's state, which closing clears.
    const [target, account, creating] = [list, accountId, mode === 'create'];
    onclose();
    if (creating) await tasks.createList(account, value);
    else if (target && value !== target.name) await tasks.renameList(target, value);
  }

  function act(run: (l: TaskList) => void) {
    const target = list;
    onclose();
    if (target) run(target);
  }
</script>

<Popover {x} {y} {align} {onclose} width={272}>
  {#if mode === 'menu' && list}
    {#if showCompletedToggle && viewing}
      <button class="menu-item" onclick={() => { tasks.showCompleted = !tasks.showCompleted; onclose(); }}>
        {#if tasks.showCompleted}<EyeOff size={15} />{t('tasks.listMenu.hideCompleted')}{:else}<Eye size={15} />{t('tasks.listMenu.showCompleted')}{/if}
      </button>
    {/if}
    {#if renamable}
      <button class="menu-item" onclick={() => go('rename')}><Pen size={15} />{t('tasks.listMenu.rename')}</button>
    {/if}
    {#if viewing && tasks.done.length && !list.readOnly}
      <button class="menu-item" onclick={() => go('clear')}><BrushCleaning size={15} />{t('tasks.listMenu.clearCompleted')}</button>
    {/if}
    {#if deletable}
      <div class="divider"></div>
      <button class="menu-item danger" onclick={() => go('delete')}><Trash size={15} />{t('tasks.listMenu.delete')}</button>
    {/if}
    {#if list.readOnly}
      <p class="note">{t('tasks.listMenu.readOnly')}</p>
    {:else if list.isDefault && list.accountId != null}
      <p class="note">{t('tasks.listMenu.defaultNote', { account: account ?? '' })}</p>
    {/if}
  {:else if mode === 'rename' || mode === 'create'}
    <form class="form" onsubmit={submit}>
      <label class="menu-label" for="task-list-name">
        {mode === 'create' ? (account ? t('tasks.listMenu.newListIn', { account }) : t('tasks.listMenu.newList')) : t('tasks.listMenu.rename')}
      </label>
      <input
        id="task-list-name"
        class="input"
        bind:this={field}
        bind:value={name}
        placeholder={t('tasks.listMenu.namePlaceholder')}
        spellcheck="false"
        onkeydown={(e) => e.key === 'Escape' && (e.stopPropagation(), onclose())}
      />
      <div class="btns">
        <button type="button" class="btn sm" onclick={onclose}>{t('common.cancel')}</button>
        <button type="submit" class="btn sm primary" disabled={!name.trim()}>{mode === 'create' ? t('tasks.listMenu.create') : t('common.save')}</button>
      </div>
    </form>
  {:else if mode === 'delete' && list}
    <div class="form">
      <p class="ask">
        {account
          ? t('tasks.listMenu.deleteAskAccount', { list: listName(list), account })
          : t('tasks.listMenu.deleteAsk', { list: listName(list) })}
      </p>
      <div class="btns">
        <button class="btn sm" onclick={onclose}>{t('common.cancel')}</button>
        <button class="btn sm danger" onclick={() => act((l) => tasks.removeList(l))}>{t('tasks.listMenu.deleteConfirm')}</button>
      </div>
    </div>
  {:else if mode === 'clear' && list}
    <div class="form">
      <p class="ask">{t('tasks.listMenu.clearAsk', { count: tasks.done.length })}</p>
      <div class="btns">
        <button class="btn sm" onclick={onclose}>{t('common.cancel')}</button>
        <button class="btn sm danger" onclick={() => act((l) => tasks.clearCompleted(l))}>{t('tasks.listMenu.clearConfirm')}</button>
      </div>
    </div>
  {/if}
</Popover>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 2px 6px 6px;
  }
  .form .menu-label {
    padding: 6px 0 0;
  }
  .btns {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .ask {
    margin: 4px 0 2px;
    line-height: 1.5;
    color: var(--text-2);
  }
  .note {
    margin: 4px 10px 6px;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--text-3);
  }
</style>
