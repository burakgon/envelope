import { api } from '../api';
import type { AiConfig, AiEvent, AiRun, AiTask } from '../api';

class Ai {
  config = $state<AiConfig | null>(null);
  /** Cached thread summaries by thread id. */
  summaries = new Map<number, string>();

  ready = $derived(!!this.config?.enabled && (this.config.hasKey || this.config.local || !this.needsKey));
  private get needsKey() {
    return !['ollama', 'lmstudio', 'custom'].includes(this.config?.provider ?? '');
  }

  async load() {
    try {
      this.config = await api.ai.config();
    } catch {
      this.config = null;
    }
  }

  run(task: AiTask, onEvent?: (e: AiEvent) => void): AiRun {
    return api.ai.run(task, onEvent);
  }
}

export const ai = new Ai();
