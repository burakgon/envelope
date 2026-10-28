<script lang="ts">
  let { size = 24 }: { size?: number } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 1024 1024" aria-hidden="true">
  <defs>
    <linearGradient id="lg-bg" x1="150" y1="80" x2="880" y2="960" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#8B7CFF" />
      <stop offset="0.5" stop-color="#6450F5" />
      <stop offset="1" stop-color="#3F2BD0" />
    </linearGradient>
  </defs>
  <rect x="40" y="40" width="944" height="944" rx="230" fill="url(#lg-bg)" />
  <rect x="226" y="300" width="572" height="424" rx="72" fill="#fff" />
  <path d="M268 356 L512 552 L756 356" fill="none" stroke="#5a46ee" stroke-width="52" stroke-linecap="round" stroke-linejoin="round" />
</svg>
