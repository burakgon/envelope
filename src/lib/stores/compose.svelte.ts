import { api } from '../api';
import type { Address, DraftInput } from '../api';
import { mail } from './mail.svelte';
import { errorMessage, toasts } from './toasts.svelte';
import { ui } from './ui.svelte';
import { isEncrypted, pgp } from './pgp.svelte';

export interface ComposerState {
  key: number;
  draftId: number | null;
  data: DraftInput;
  /** Reply composer shown inside the conversation instead of floating. */
  inline: boolean;
  threadId: number | null;
  minimized: boolean;
  maximized: boolean;
  showCc: boolean;
  showBcc: boolean;
  saving: boolean;
  savedAt: number | null;
  /** Bumped to ask the editor to focus. */
  focus: number;
}

let keySeq = 1;

function emptyDraft(accountId: number): DraftInput {
  return { accountId, to: [], cc: [], bcc: [], subject: '', html: '', attachments: [], mode: 'new' };
}

function parseMailto(url: string): Partial<DraftInput> {
  try {
    const rest = url.replace(/^mailto:/i, '');
    const [addrPart, query = ''] = rest.split('?');
    const params = new URLSearchParams(query);
    const list = (s: string | null): Address[] =>
      (s ?? '')
        .split(',')
        .map((x) => decodeURIComponent(x.trim()))
        .filter(Boolean)
        .map((email) => ({ email }));
    const body = params.get('body');
    return {
      to: list(addrPart),
      cc: list(params.get('cc')),
      bcc: list(params.get('bcc')),
      subject: params.get('subject') ?? '',
      html: body ? body.split('\n').map((l) => `<p>${l.replace(/&/g, '&amp;').replace(/</g, '&lt;')}</p>`).join('') : '',
    };
  } catch {
    return {};
  }
}

class Compose {
  list = $state<ComposerState[]>([]);
  private timers = new Map<number, ReturnType<typeof setTimeout>>();

  floating = $derived(this.list.filter((c) => !c.inline));

  defaultAccount(): number {
    const mb = mail.mailbox;
    if (mb.kind === 'folder') {
      const f = mail.folderById.get(mb.id);
      if (f) return f.accountId;
    }
    if (mail.detail) return mail.detail.thread.accountId;
    return mail.accounts[0]?.id ?? 0;
  }

  private withSignature(data: DraftInput): DraftInput {
    if (data.html || data.mode !== 'new') return data;
    const identity =
      mail.identities.find((i) => i.id === data.identityId) ??
      mail.identities.find((i) => i.accountId === data.accountId && i.isDefault);
    if (identity?.signature) data.html = `<p></p><div class="envelope-signature">${identity.signature}</div>`;
    return data;
  }

  open(data: DraftInput, opts: Partial<ComposerState> = {}): ComposerState {
    const c: ComposerState = {
      key: keySeq++,
      draftId: null,
      data,
      inline: false,
      threadId: null,
      minimized: false,
      maximized: false,
      showCc: data.cc.length > 0,
      showBcc: data.bcc.length > 0,
      saving: false,
      savedAt: null,
      // New messages start in the To field; replies start in the editor.
      focus: data.to.length === 0 ? 0 : 1,
      ...opts,
    };
    // Only one inline reply per conversation.
    if (c.inline) this.list = this.list.filter((x) => !(x.inline && x.threadId === c.threadId));
    // Expanded floating composers stack; collapse older ones.
    if (!c.inline) for (const x of this.list) if (!x.inline) x.minimized = true;
    this.list.push(c);
    return this.list[this.list.length - 1];
  }

  new(prefill: Partial<DraftInput> = {}) {
    if (mail.accounts.length === 0) {
      ui.open('addAccount');
      return;
    }
    const data = this.withSignature({ ...emptyDraft(prefill.accountId ?? this.defaultAccount()), ...prefill });
    this.open(data);
  }

  fromMailto(url: string) {
    this.new(parseMailto(url));
  }

  async reply(messageId: number, mode: 'reply' | 'replyAll' | 'forward', inline = true) {
    try {
      // An encrypted original is decrypted first, so the reply can quote it.
      const original = mail.detail?.messages.find((m) => m.id === messageId);
      if (original && isEncrypted(original)) await pgp.open(messageId);
      const data = await api.composeReply(messageId, mode);
      const threadId = mail.detail?.thread.id ?? null;
      this.open(data, { inline, threadId, showCc: data.cc.length > 0 });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async openDraft(draftId: number) {
    const existing = this.list.find((c) => c.draftId === draftId);
    if (existing) {
      existing.minimized = false;
      existing.focus++;
      return;
    }
    const d = await api.draft(draftId);
    this.open(d.data, { draftId });
  }

  get(key: number): ComposerState | undefined {
    return this.list.find((c) => c.key === key);
  }

  changed(key: number) {
    const t = this.timers.get(key);
    if (t) clearTimeout(t);
    this.timers.set(key, setTimeout(() => this.save(key, false), 1200));
  }

  async save(key: number, upload: boolean) {
    const c = this.get(key);
    if (!c) return;
    const empty = !c.data.subject && c.data.to.length === 0 && !c.data.html.replace(/<[^>]*>|\s|&nbsp;/g, '') && c.data.attachments.length === 0;
    if (empty && c.draftId == null) return;
    c.saving = true;
    try {
      c.draftId = await api.saveDraft(c.draftId, $state.snapshot(c.data) as DraftInput, upload);
      c.savedAt = Date.now();
    } catch (e) {
      toasts.error(`Could not save draft: ${errorMessage(e)}`);
    } finally {
      c.saving = false;
    }
  }

  private remove(key: number) {
    const t = this.timers.get(key);
    if (t) clearTimeout(t);
    this.timers.delete(key);
    this.list = this.list.filter((c) => c.key !== key);
  }

  async close(key: number) {
    const c = this.get(key);
    if (!c) return;
    this.remove(key);
    await this.save(c.key, true).catch(() => {});
    if (c.draftId != null) {
      const draftId = c.draftId;
      await api.saveDraft(draftId, $state.snapshot(c.data) as DraftInput, true).catch(() => {});
      toasts.show('Draft saved', {
        action: { label: 'Open', run: () => this.openDraft(draftId) },
      });
    }
  }

  async discard(key: number) {
    const c = this.get(key);
    if (!c) return;
    this.remove(key);
    if (c.draftId != null) await api.deleteDraft(c.draftId).catch(() => {});
    toasts.show('Draft discarded');
  }

  async send(key: number, sendAt: number | null = null) {
    const c = this.get(key);
    if (!c) return;
    const data = $state.snapshot(c.data) as DraftInput;
    if (data.to.length + data.cc.length + data.bcc.length === 0) {
      toasts.error('Add at least one recipient');
      return;
    }
    if (!data.subject.trim() && !confirm('Send this message without a subject?')) return;
    const replyThread = c.threadId;
    this.remove(key);
    try {
      const delay = sendAt ? 0 : ui.settings.undoSendSeconds * 1000;
      const outboxId = await api.send(c.draftId, data, delay, sendAt);
      const label = sendAt
        ? `Scheduled for ${new Date(sendAt).toLocaleString(undefined, { weekday: 'short', hour: 'numeric', minute: '2-digit' })}`
        : 'Sending…';
      toasts.undoable(
        label,
        async () => {
          const draftId = await api.cancelSend(outboxId);
          this.open(data, { draftId, inline: c.inline, threadId: c.threadId });
          toasts.show('Sending cancelled');
        },
        sendAt ? 8000 : Math.max(3000, delay),
      );
      if (ui.settings.sendAndArchive && replyThread != null && data.mode !== 'forward') {
        setTimeout(() => mail.archive([replyThread]), 300);
      }
    } catch (e) {
      toasts.error(errorMessage(e));
      this.open(data, { draftId: c.draftId, inline: c.inline, threadId: c.threadId });
    }
  }
}

export const compose = new Compose();
