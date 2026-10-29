import { api } from '../api';
import type { Address, Draft, DraftInput, MessageView, ThreadSummary } from '../api';
import { mail } from './mail.svelte';
import { dialogs } from './dialogs.svelte';
import { errorMessage, toasts } from './toasts.svelte';
import { ui } from './ui.svelte';
import { t } from '../i18n';
import { dateFormat } from '../utils/format';
import { isEncrypted, pgp } from './pgp.svelte';

export interface ComposerState {
  key: number;
  draftId: number | null;
  data: DraftInput;
  /** Reply composer shown inside the conversation instead of floating. */
  inline: boolean;
  threadId: number | null;
  /** The draft message (in Drafts or a conversation) this composer edits, while it is listed. */
  sourceMessageId: number | null;
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
  /** Composers asking whether to send without a subject. */
  private asking = new Set<number>();

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
      sourceMessageId: null,
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

  /** Opens a draft for editing (or brings its composer forward): inline under its conversation when that is open. */
  async openDraft(draft: number | Draft, opts: { threadId?: number | null } = {}) {
    const id = typeof draft === 'number' ? draft : draft.id;
    const existing = this.list.find((c) => c.draftId === id);
    if (existing) {
      existing.minimized = false;
      existing.focus++;
      return;
    }
    try {
      const d = typeof draft === 'number' ? await api.draft(draft) : draft;
      const reply = d.data.mode === 'reply' || d.data.mode === 'replyAll';
      const thread = mail.detail?.thread.id ?? null;
      const inThread = thread != null && (opts.threadId ?? null) === thread;
      const replyHere = reply && d.data.replyToMessageId != null && !!mail.detail?.messages.some((m) => m.id === d.data.replyToMessageId);
      // Inline under the open conversation, unless another reply is being written there.
      const inline = (inThread || replyHere) && reply && !this.list.some((c) => c.inline && c.threadId === thread);
      this.open(d.data, {
        draftId: d.id,
        sourceMessageId: d.serverMessageId,
        inline,
        threadId: inline ? thread : null,
        focus: 1,
        savedAt: d.updatedAt,
      });
    } catch (e) {
      toasts.error(t('compose.draftOpenFailed', { error: errorMessage(e) }));
    }
  }

  /** Opens a draft message (from Drafts, or in a conversation) in a composer to edit it. */
  async editMessage(m: MessageView) {
    const open = this.list.find((c) => c.sourceMessageId === m.id);
    if (open) {
      open.minimized = false;
      open.focus++;
      return;
    }
    try {
      if (isEncrypted(m)) await pgp.open(m.id);
      const d = await api.draftForMessage(m.id);
      await this.openDraft(d, { threadId: m.threadId });
    } catch (e) {
      toasts.error(t('compose.draftOpenFailed', { error: errorMessage(e) }));
    }
  }

  /**
   * Opens a row of the message list: a draft only on this computer in its composer; in Drafts,
   * the conversation with its draft open to be edited; anything else in the reader.
   */
  async openRow(th: ThreadSummary) {
    if (th.localDraftId != null) {
      mail.select(th.id);
      await this.openDraft(th.localDraftId);
      return;
    }
    if (!mail.inDrafts || th.draftCount === 0) {
      mail.select(th.id);
      return;
    }
    mail.select(th.id, { open: false });
    await mail.openThread(th.id);
    if (mail.selectedId === th.id) await this.editThread(th.id);
  }

  /** Opens the newest draft of a conversation, if it has one; true when it did. */
  async editThread(threadId: number): Promise<boolean> {
    try {
      const d = mail.detail?.thread.id === threadId ? mail.detail : await api.thread(threadId);
      const draft = [...d.messages].reverse().find((m) => m.flags.draft);
      if (!draft) return false;
      await this.editMessage(draft);
      return true;
    } catch (e) {
      toasts.error(errorMessage(e));
      return false;
    }
  }

  get(key: number): ComposerState | undefined {
    return this.list.find((c) => c.key === key);
  }

  changed(key: number) {
    const timer = this.timers.get(key);
    if (timer) clearTimeout(timer);
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
      mail.draftsChanged();
    } catch (e) {
      toasts.error(t('compose.saveFailed', { error: errorMessage(e) }));
    } finally {
      c.saving = false;
    }
  }

  private remove(key: number) {
    const timer = this.timers.get(key);
    if (timer) clearTimeout(timer);
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
      toasts.show(t('compose.draftSaved'), {
        action: { label: t('common.open'), run: () => this.openDraft(draftId) },
      });
    }
  }

  async discard(key: number) {
    const c = this.get(key);
    if (!c) return;
    this.remove(key);
    if (c.draftId != null) await api.deleteDraft(c.draftId).catch(() => {});
    toasts.show(t('compose.draftDiscarded'));
  }

  async send(key: number, sendAt: number | null = null) {
    const c = this.get(key);
    if (!c) return;
    const data = $state.snapshot(c.data) as DraftInput;
    if (data.to.length + data.cc.length + data.bcc.length === 0) {
      toasts.error(t('compose.noRecipients'));
      return;
    }
    if (!data.subject.trim()) {
      // Asked once, however often Send is pressed meanwhile.
      if (this.asking.has(key)) return;
      this.asking.add(key);
      const ok = await dialogs.confirm({ title: t('compose.confirmNoSubject'), confirmLabel: t('common.send') }).finally(() => this.asking.delete(key));
      if (!ok || !this.get(key)) return;
    }
    const replyThread = c.threadId;
    this.remove(key);
    try {
      const delay = sendAt ? 0 : ui.settings.undoSendSeconds * 1000;
      const outboxId = await api.send(c.draftId, data, delay, sendAt);
      const label = sendAt
        ? t('compose.scheduledFor', { time: dateFormat({ weekday: 'short', hour: 'numeric', minute: '2-digit' }).format(sendAt) })
        : t('compose.sending');
      toasts.undoable(
        label,
        async () => {
          const draftId = await api.cancelSend(outboxId);
          this.open(data, { draftId, inline: c.inline, threadId: c.threadId });
          toasts.show(t('compose.sendingCancelled'));
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
