import { Injectable, signal } from '@angular/core';

import type { AdversarialReport, JobRequirements, ResumeDataset } from '../models';

/** Everything the chat panel needs to hold a grounded conversation about the
 *  build the workspace has open: the recorded dataset (the honest background),
 *  the parsed JD the conversation is about, and — when the build has been
 *  tailored/reviewed — its canonical draft and adversarial report. `canonical`
 *  and `report` are null for a JD-only (pre-tailor) context. */
export interface ChatContext {
  buildId: string;
  title: string;
  dataset: ResumeDataset;
  jd: JobRequirements;
  canonical: unknown | null;
  report: AdversarialReport | null;
}

/** The bridge between the tailoring workspace (which loads a build's dataset,
 *  JD, draft, and report) and the chat panel, which lives up in the app shell —
 *  a sibling of the router outlet, so it can't read the routed component's
 *  signals directly. The workspace pushes its loaded context here as it changes
 *  and clears it on the way out; the panel reads it to drive `chat_reply`. When
 *  the context is null (no build open) the panel shows its idle prompt. */
@Injectable({ providedIn: 'root' })
export class ChatStore {
  /** The open build's chat context, or null when no build is open. */
  readonly context = signal<ChatContext | null>(null);

  setContext(context: ChatContext | null): void {
    this.context.set(context);
  }
}
