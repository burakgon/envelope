import type { LinkTarget } from '../utils/links';

/** The link pointed at or focused in a message, shown in a pill as browsers show it in their status bar. */
class LinkStatus {
  current = $state<LinkTarget | null>(null);
  private owner: object | null = null;

  show(owner: object, target: LinkTarget | null) {
    this.owner = target ? owner : null;
    this.current = target;
  }

  /** Hides the pill if `owner` (a message frame) put it up. */
  hide(owner: object) {
    if (this.owner !== owner) return;
    this.owner = null;
    this.current = null;
  }
}

export const linkStatus = new LinkStatus();
