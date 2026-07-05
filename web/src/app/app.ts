import { Component, OnInit, inject, signal } from '@angular/core';
import { RouterOutlet } from '@angular/router';

import { Topbar } from './shell/topbar/topbar';
import { Sidebar } from './shell/sidebar/sidebar';
import { ChatPanel } from './shell/chat-panel/chat-panel';
import { BuildsStore } from './services/builds-store';
import { WasmService } from './services/wasm.service';
import { CopilotOverlay } from './shared/copilot-host';

const SIDEBAR_COLLAPSED_KEY = 'aarg.sidebar-collapsed';
const CHAT_OPEN_KEY = 'aarg.chat.open';
const CHAT_WIDTH_KEY = 'aarg.chat.width';
const CHAT_DEFAULT_WIDTH = 380;
const CHAT_MIN_WIDTH = 300;
const CHAT_MAX_WIDTH = 640;

/** Reads the persisted desktop collapse choice. Guarded for contexts where
 *  `localStorage` doesn't exist. */
function loadSidebarCollapsed(): boolean {
  if (typeof localStorage === 'undefined') return false;
  return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === '1';
}

/** The persisted chat-open choice (default closed). */
function loadChatOpen(): boolean {
  if (typeof localStorage === 'undefined') return false;
  return localStorage.getItem(CHAT_OPEN_KEY) === '1';
}

/** The persisted chat width, clamped to the resize bounds (default 380px). */
function loadChatWidth(): number {
  if (typeof localStorage === 'undefined') return CHAT_DEFAULT_WIDTH;
  const raw = Number(localStorage.getItem(CHAT_WIDTH_KEY));
  if (!Number.isFinite(raw) || raw <= 0) return CHAT_DEFAULT_WIDTH;
  return Math.min(CHAT_MAX_WIDTH, Math.max(CHAT_MIN_WIDTH, raw));
}

/** App shell: the sticky topbar over a two-column grid of the recent-builds
 *  sidebar and the routed main panel. At ≤1080px the sidebar becomes a drawer
 *  toggled from the topbar hamburger, with a dismiss scrim. Above 1080px the
 *  sidebar column can instead be collapsed to a slim rail via a control on
 *  the sidebar itself; that choice persists across sessions. */
@Component({
  selector: 'app-root',
  imports: [RouterOutlet, Topbar, Sidebar, ChatPanel, CopilotOverlay],
  templateUrl: './app.html',
  styleUrl: './app.css',
  host: { '(document:keydown.escape)': 'onEscape()' },
})
export class App implements OnInit {
  private readonly store = inject(BuildsStore);
  private readonly wasm = inject(WasmService);

  /** Drawer open state (only meaningful ≤1080px). */
  protected readonly navOpen = signal(false);

  /** Whether the chat panel is open — a third column on desktop, a full-width
   *  overlay ≤1080px. Persisted so the choice survives a reload. */
  protected readonly chatOpen = signal(loadChatOpen());
  /** The chat panel's desktop width (px), driven by its drag handle and
   *  persisted. Ignored ≤1080px, where the panel overlays full-width. */
  protected readonly chatWidth = signal(loadChatWidth());

  /** Whether a resize drag is in progress, so the shell can suppress the grid
   *  column transition (it would rubber-band against the live drag). */
  protected readonly chatResizing = signal(false);

  /** Escape dismisses the mobile drawer or the chat, whichever is open. The chat
   *  yields to any open dialog-like surface (the copilot Q&A, the refine drawer,
   *  the edit-history popover, the delete confirm all mark themselves
   *  `role="dialog"`/`"alertdialog"`), so one Escape that dismisses such a layer
   *  does not also slam the chat shut. No-ops otherwise. */
  protected onEscape(): void {
    if (this.navOpen()) {
      this.navOpen.set(false);
      return;
    }
    if (this.chatOpen() && !document.querySelector('[role="dialog"], [role="alertdialog"]')) {
      this.setChatOpen(false);
    }
  }

  protected closeNav(): void {
    if (this.navOpen()) this.navOpen.set(false);
  }

  protected toggleChat(): void {
    this.setChatOpen(!this.chatOpen());
  }

  private setChatOpen(open: boolean): void {
    this.chatOpen.set(open);
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem(CHAT_OPEN_KEY, open ? '1' : '0');
    }
  }

  /** Live width during a drag: update the column but do not persist yet (that
   *  would write localStorage on every pointermove). Marks a drag in progress so
   *  the shell drops the grid transition and the column tracks the pointer 1:1. */
  protected onChatWidth(width: number): void {
    this.chatResizing.set(true);
    this.chatWidth.set(width);
  }

  /** Drag released: adopt the final width, persist it once, and re-enable the
   *  column transition. */
  protected onChatWidthCommit(width: number): void {
    this.chatWidth.set(width);
    this.chatResizing.set(false);
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem(CHAT_WIDTH_KEY, String(Math.round(width)));
    }
  }

  /** Desktop sidebar collapse (only meaningful above 1080px; ignored by the
   *  ≤1080px drawer, which always opens full-width regardless). */
  protected readonly collapsed = signal(loadSidebarCollapsed());

  protected toggleCollapsed(): void {
    const next = !this.collapsed();
    this.collapsed.set(next);
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem(SIDEBAR_COLLAPSED_KEY, next ? '1' : '0');
    }
  }

  ngOnInit(): void {
    this.store.load();
    // Warm the deterministic wasm core in the background so the first tailoring
    // action is instant. The .wasm is a same-origin static asset, so this needs
    // no backend; failures are non-fatal (the core loads on demand regardless).
    this.wasm.normalizeDashes('').catch(() => {});
  }
}
