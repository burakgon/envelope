<script lang="ts">
  // A birthday from the address books: whose, when, how old they turn, and their contact page.
  import type { Occurrence } from '../../api';
  import { cal } from '../../stores/calendar.svelte';
  import { ageIn, BIRTHDAY_COLOR } from '../../utils/birthdays';
  import { people } from '../../stores/contacts.svelte';
  import { ui } from '../../stores/ui.svelte';
  import { t } from '../../i18n';
  import { dateFormat } from '../../utils/format';
  import { BookUser, Cake } from '../../icons';
  import Popover from '../Popover.svelte';

  let { occurrence: o, rect, onclose }: { occurrence: Occurrence; rect: DOMRect; onclose: () => void } = $props();

  const contactId = $derived(-o.eventId);
  const birthday = $derived(cal.birthdays.find((b) => b.contactId === contactId));
  const age = $derived(birthday ? ageIn(birthday, new Date(o.start).getFullYear()) : null);

  async function openContact() {
    onclose();
    ui.view = 'contacts';
    await people.init();
    people.select(contactId);
  }
</script>

<Popover x={rect.left} y={rect.bottom + 4} width={280} {onclose}>
  <div class="birthday">
    <span class="icon" style:--c={BIRTHDAY_COLOR}><Cake size={18} /></span>
    <div class="text">
      <b>{birthday?.name ?? o.title}</b>
      <span>{dateFormat({ weekday: 'long', month: 'long', day: 'numeric' }).format(o.start)}</span>
      {#if age != null}<span>{t('birthdays.turns', { age })}</span>{/if}
    </div>
  </div>
  <button class="menu-item" onclick={openContact}><BookUser size={15} />{t('birthdays.openContact')}</button>
</Popover>

<style>
  .birthday {
    display: flex;
    gap: 10px;
    padding: 8px 10px 10px;
  }
  .icon {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 10px;
    color: var(--c);
    background: color-mix(in oklab, var(--c) 14%, transparent);
  }
  .text {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .text b {
    font-weight: 650;
  }
  .text span {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
</style>
