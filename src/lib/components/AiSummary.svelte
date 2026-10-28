<script lang="ts">
  import type { MessageView } from '../api';

  let { threadId, messages }: { threadId: number; messages: MessageView[] } = $props();
  void threadId;
  void messages;
</script>
