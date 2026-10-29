<script lang="ts">
  import { Download, X } from '../icons';
  import { t } from '../i18n';

  // Only part of the browser demo: points visitors at the real app.
  let hidden = $state(sessionStorage.getItem('envelope.demo.banner') === 'hidden');

  function close() {
    hidden = true;
    sessionStorage.setItem('envelope.demo.banner', 'hidden');
  }
</script>

{#if !hidden}
  <div class="banner" role="note">
    <button class="close" aria-label={t('demo.hide')} onclick={close}><X size={13} /></button>
    <div class="head"><span class="dot"></span><b>{t('demo.title')}</b></div>
    <p>{t('demo.body')}</p>
    <a class="get" href="https://github.com/burakgon/envelope/releases/latest" target="_blank" rel="noopener"><Download size={14} />{t('demo.download')}</a>
    <a class="gh" href="https://github.com/burakgon/envelope" target="_blank" rel="noopener">{t('demo.star')}</a>
  </div>
{/if}

<style>
  .banner {
    position: relative;
    margin: 0 10px 10px;
    padding: 12px;
    border-radius: 12px;
    font-size: var(--text-sm);
    color: var(--text-2);
    background: var(--bg-elevated);
    box-shadow: inset 0 0 0 1px var(--border), var(--shadow-sm);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--green);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--green) 25%, transparent);
  }
  b {
    color: var(--text);
  }
  p {
    margin: 0 0 4px;
    line-height: 1.4;
  }
  a {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    height: 30px;
    border-radius: 9px;
    font-weight: 600;
    text-decoration: none;
  }
  .get {
    color: var(--accent-fg);
    background: var(--accent-solid);
  }
  .get:hover {
    background: var(--accent-solid-hover);
  }
  .gh {
    color: var(--text);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .gh:hover {
    background: var(--bg-hover);
  }
  .close {
    position: absolute;
    top: 8px;
    right: 8px;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 7px;
    color: var(--text-3);
  }
  .close:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
</style>
