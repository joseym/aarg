// Repeatable browser screenshots for AARG's README / docs, driven against a
// *served committed fixture* (docs/demo/workspace/) — no Anthropic auth, no
// keychain, no live model calls. It just reads the fixture read-only through
// `aarg serve` and photographs the browser UI.
//
// Usage:
//   AARG_DIR=<scratch-with-.aarg> aarg serve --dir web/dist/aarg/browser --port 8799 &
//   node <pwtest>/... docs/demo/capture-screenshots.mjs
//
// Env knobs:
//   AARG_URL   base URL of the running server   (default http://127.0.0.1:8799)
//   SHOTS_DIR  output directory for the PNGs     (default docs/screenshots)
//
// Playwright: this script needs `chromium` from a Playwright install. ESM does
// not honour NODE_PATH, so resolution is: try a plain `import('playwright')`
// first (works when playwright is installed next to this repo), and otherwise
// fall back to `$PLAYWRIGHT_DIR/playwright/index.mjs`. The install used to
// author it lives at
//   /private/tmp/claude-501/.../scratchpad/pwtest/node_modules
// so invoke with e.g.
//   PLAYWRIGHT_DIR=/private/tmp/claude-501/.../scratchpad/pwtest/node_modules \
//     node docs/demo/capture-screenshots.mjs
// (or `npm i playwright` first). It is dependency-light on purpose: one
// dynamic import, no config, no test runner.

import { mkdir } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

async function loadChromium() {
  const attempts = ['playwright'];
  if (process.env.PLAYWRIGHT_DIR) {
    attempts.push(pathToFileURL(`${process.env.PLAYWRIGHT_DIR}/playwright/index.mjs`).href);
  }
  for (const spec of attempts) {
    try {
      return (await import(spec)).chromium;
    } catch {
      /* try the next candidate */
    }
  }
  throw new Error(
    'playwright not found — `npm i playwright`, or point PLAYWRIGHT_DIR at a node_modules dir that has it',
  );
}

const chromium = await loadChromium();

const BASE = process.env.AARG_URL ?? 'http://127.0.0.1:8799';
const SHOTS = process.env.SHOTS_DIR ?? 'docs/screenshots';

// If any of these ever appear in the rendered DOM, the fixture leaked real
// data — fail loudly rather than commit a screenshot with PII. Keep this in
// sync with the fixture denylist grep in the record-demos skill.
const DENYLIST = [
  'josey', 'morton', 'joseymorton', 'claremore', 'prometheum', 'scoutgroup',
  'consumeraffairs', 'cloudload', 'octobear', 'coreweave', 'sambanova',
  'pantheon', 'hightouch', 'thumbtack', 'mainstay', 'syndio', 'finra',
  'broker-dealer', 'spbd',
];

/** Attach a console/pageerror collector to a page; returns the error array. */
function watchErrors(page, label) {
  const errors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(`[${label}] console: ${msg.text()}`);
  });
  page.on('pageerror', (err) => errors.push(`[${label}] pageerror: ${err.message}`));
  return errors;
}

/** Wait for the résumé preview (candidate name) to render — the SPA + wasm
 *  pipeline has finished loading the newest build by then. */
async function waitForPreview(page) {
  await page.waitForSelector('.p-name', { timeout: 30_000 });
  // Give the count-up score animation and layout a beat to settle.
  await page.waitForTimeout(800);
}

/** Grab the page's rendered text (lower-cased) so the denylist guard can be
 *  run over *every* captured page state, not just the candidate/company text
 *  from the first page. Never throws — a missing body just yields ''. */
async function snapText(page) {
  try {
    return (await page.locator('body').innerText()).toLowerCase();
  } catch {
    return '';
  }
}

async function main() {
  await mkdir(SHOTS, { recursive: true });
  // `channel: 'chromium'` uses the *full* browser, not the headless-shell that
  // `launch()` defaults to. The shell can't paint a `<iframe src="blob:…">`
  // PDF (no PDFium plugin), which would leave pixel.png blank — the full
  // browser's new-headless mode renders it. `npx playwright install chromium`
  // provides this channel alongside the shell.
  const browser = await chromium.launch({ channel: 'chromium' });
  const allErrors = [];
  const pageTexts = []; // rendered text of every extra page state, for the denylist guard
  let seenName = '';
  let seenCompanies = [];

  try {
    // --- mobile.png : phone viewport, lands on the newest build ---------
    {
      const ctx = await browser.newContext({
        viewport: { width: 430, height: 932 },
        deviceScaleFactor: 2,
      });
      const page = await ctx.newPage();
      const errors = watchErrors(page, 'mobile');
      await page.goto(BASE, { waitUntil: 'networkidle' });
      await waitForPreview(page);
      seenName = (await page.locator('.p-name').first().textContent())?.trim() ?? '';
      seenCompanies = await page.locator('.p-co').allTextContents();
      await page.screenshot({ path: `${SHOTS}/mobile.png` }); // above-the-fold
      allErrors.push(...errors);
      await ctx.close();
    }

    // --- desktop.png : the three-pane workspace -------------------------
    const deskCtx = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      deviceScaleFactor: 2,
    });
    {
      const page = await deskCtx.newPage();
      const errors = watchErrors(page, 'desktop');
      await page.goto(BASE, { waitUntil: 'networkidle' });
      await waitForPreview(page);
      await page.screenshot({ path: `${SHOTS}/desktop.png` });
      allErrors.push(...errors);
      await page.close();
    }

    // --- coverage.png : the Coverage-map on a PHONE, its card layout --------
    // The README pairs this beside mobile.png at width 300, where the old
    // desktop-width table shrank to an unreadable strip. Recaptured at the
    // phone viewport, each requirement renders as a distinct state-railed card
    // (coverage-map's `max-width: 1080px` rules), so the "on a phone, too"
    // pairing is both honest and legible.
    {
      const covCtx = await browser.newContext({
        viewport: { width: 430, height: 932 },
        deviceScaleFactor: 2,
      });
      const page = await covCtx.newPage();
      const errors = watchErrors(page, 'coverage');
      await page.goto(BASE, { waitUntil: 'networkidle' });
      await waitForPreview(page);
      await page.locator('.segmented button', { hasText: 'Coverage map' }).click();
      await page.waitForSelector('.cov-map .req-row', { timeout: 15_000 });
      await page.waitForTimeout(500);
      // Scroll the first requirement card near the top so 2–3 cards fill the
      // frame (each is tall on mobile), then shoot the full phone viewport.
      await page.locator('.cov-map .req-row').first().evaluate((el) => {
        el.scrollIntoView({ block: 'start' });
        window.scrollBy(0, -70); // a little breathing room above the top card
      });
      await page.waitForTimeout(300);
      await page.screenshot({ path: `${SHOTS}/coverage.png` });
      pageTexts.push(await snapText(page));
      allErrors.push(...errors);
      await covCtx.close();
    }

    // --- pixel.png : the real Typst PDF in the Final-preview pane -----------
    // Final preview → Pixel-perfect sub-toggle → the fixture server renders
    // the résumé through Typst (no model key needed) and hands back a blob PDF.
    // The frame shows the template picker sitting above the rendered page.
    {
      const page = await deskCtx.newPage();
      const errors = watchErrors(page, 'pixel');
      await page.goto(BASE, { waitUntil: 'networkidle' });
      await waitForPreview(page);
      await page.locator('app-view-toggle .segmented button', { hasText: 'Final preview' }).click();
      await page.locator('.pv-sub .segmented button', { hasText: 'Pixel-perfect' }).click();
      // The <iframe src="blob:…"> only exists once /api/render → Typst returns;
      // :not(.hide) rules out the stays-mounted-but-hidden editing state.
      await page.waitForSelector('app-pdf-preview:not(.hide) iframe.pdf[src^="blob:"]', {
        timeout: 40_000,
      });
      await page.waitForTimeout(1500); // let the PDF paint inside the iframe
      await page.waitForTimeout(800); // + a beat for PDFium to apply #view=FitH
      // Frame the fidelity sub-toggle (Editing | Pixel-perfect) + template
      // picker down through the rendered Typst PDF, which — now that the blob
      // URL hides the viewer's toolbar and nav pane (#toolbar=0&navpanes=0) —
      // fills the frame as a document with no dark viewer dead space. Clip from
      // the sub-toggle's top-left to the iframe's bottom rather than element-
      // shooting the whole column, so the shot is the feature, not the pane.
      const sub = await page.locator('.pv-sub').boundingBox();
      const frame = await page
        .locator('app-pdf-preview:not(.hide) iframe.pdf')
        .boundingBox();
      const vp = page.viewportSize();
      const x = Math.max(0, Math.floor(sub.x));
      const y = Math.max(0, Math.floor(sub.y));
      const right = Math.min(vp.width, Math.ceil(Math.max(sub.x + sub.width, frame.x + frame.width)));
      const bottom = Math.min(vp.height, Math.ceil(frame.y + frame.height));
      await page.screenshot({
        path: `${SHOTS}/pixel.png`,
        clip: { x, y, width: right - x, height: bottom - y },
      });
      pageTexts.push(await snapText(page));
      allErrors.push(...errors);
      await page.close();
    }

    // --- copilot.png : the Q&A modal, driven headless via the harness hook --
    // globalThis.__copilotHost.ask(envelope) opens the *real* modal without any
    // model call. We fire it without awaiting (ask() only resolves once the
    // question is answered), photograph the centered modal + backdrop, then
    // cancel() to settle the dangling promise so nothing hangs.
    {
      const page = await deskCtx.newPage();
      const errors = watchErrors(page, 'copilot');
      await page.goto(BASE, { waitUntil: 'networkidle' });
      await waitForPreview(page);
      await page.evaluate((env) => {
        const host = globalThis.__copilotHost;
        host.ask(JSON.stringify(env)).catch(() => {}); // intentionally not awaited
      }, { kind: 'text', prompt: 'Roughly how many shipments did the tracking service handle per day?' });
      await page.waitForSelector('app-copilot-overlay .modal[role="dialog"]', { timeout: 10_000 });
      await page.waitForTimeout(500);
      // Crop tight around the modal: its bounding box + ~90px of margin on all
      // sides, so a hint of the blurred workspace frames it rather than a whole
      // page of dead backdrop. Clamped to the viewport so the clip stays valid.
      const box = await page.locator('app-copilot-overlay .modal[role="dialog"]').boundingBox();
      const M = 90;
      const cvp = page.viewportSize();
      const cx = Math.max(0, Math.floor(box.x - M));
      const cy = Math.max(0, Math.floor(box.y - M));
      const cRight = Math.min(cvp.width, Math.ceil(box.x + box.width + M));
      const cBottom = Math.min(cvp.height, Math.ceil(box.y + box.height + M));
      await page.screenshot({
        path: `${SHOTS}/copilot.png`,
        clip: { x: cx, y: cy, width: cRight - cx, height: cBottom - cy },
      });
      pageTexts.push(await snapText(page));
      await page.evaluate(() => globalThis.__copilotHost.cancel()); // settle + close
      allErrors.push(...errors);
      await page.close();
    }

    // --- edit-bar.png : the sticky pending-edits bar over the document ------
    // Focus a preview line, append fixture-appropriate text, blur → the bar
    // slides up. Read-only: we never click Save (nor press Cmd/Ctrl+S).
    {
      const page = await deskCtx.newPage();
      const errors = watchErrors(page, 'edit-bar');
      await page.goto(BASE, { waitUntil: 'networkidle' });
      await waitForPreview(page);
      const line = page.locator('app-resume-preview span.prov[contenteditable="true"]').first();
      await line.click();
      // Drop the caret at the very end of the line, then type the addition.
      await line.evaluate((el) => {
        const r = document.createRange();
        r.selectNodeContents(el);
        r.collapse(false);
        const sel = window.getSelection();
        sel.removeAllRanges();
        sel.addRange(r);
      });
      await page.keyboard.type(' Cut delivery exceptions by a third.');
      await line.evaluate((el) => el.blur()); // blur emits the pending edit
      await page.waitForSelector('app-edit-bar.visible', { timeout: 10_000 });
      await page.waitForTimeout(500); // let the slide-up transition settle
      // Lower half of the viewport: the bar sitting over the document.
      const { width, height } = page.viewportSize();
      await page.screenshot({
        path: `${SHOTS}/edit-bar.png`,
        clip: { x: 0, y: Math.round(height / 2), width, height: Math.round(height / 2) },
      });
      pageTexts.push(await snapText(page));
      allErrors.push(...errors);
      await page.close();
    }

    // --- refine.png : the refine drawer with an objection + Run button ------
    // Each open objection card has a "Refine it" button; only objections that
    // target a line (not the overall verdict) render a Run button in the
    // drawer, so try cards until one shows the Run action.
    {
      const page = await deskCtx.newPage();
      const errors = watchErrors(page, 'refine');
      await page.goto(BASE, { waitUntil: 'networkidle' });
      await waitForPreview(page);
      const refineBtns = page.locator('app-reviewer-rail .card button.btn-primary', {
        hasText: 'Refine it',
      });
      const n = await refineBtns.count();
      let opened = false;
      for (let i = 0; i < n; i++) {
        await refineBtns.nth(i).click();
        await page.waitForSelector('app-refine-drawer .drawer[role="dialog"]', { timeout: 10_000 });
        await page.waitForTimeout(300);
        if ((await page.locator('app-refine-drawer .cp-foot button.btn-primary').count()) > 0) {
          opened = true;
          break;
        }
        await page.keyboard.press('Escape'); // this objection can't be run; try the next
        await page.waitForTimeout(300);
      }
      if (!opened) throw new Error('refine: no objection card exposed a runnable Run button');
      await page.waitForTimeout(400);
      await page.screenshot({ path: `${SHOTS}/refine.png` });
      pageTexts.push(await snapText(page));
      await page.keyboard.press('Escape'); // close the drawer
      allErrors.push(...errors);
      await page.close();
    }

    // --- new-build.png : the /new three-source picker + run panel -----------
    {
      const page = await deskCtx.newPage();
      const errors = watchErrors(page, 'new-build');
      await page.goto(`${BASE}/new`, { waitUntil: 'networkidle' });
      await page.waitForSelector('app-new-build .seg[role="tablist"] button.seg-btn', {
        timeout: 15_000,
      });
      await page.waitForTimeout(500);
      await page.screenshot({ path: `${SHOTS}/new-build.png` }); // above-the-fold
      pageTexts.push(await snapText(page));
      allErrors.push(...errors);
      await page.close();
    }

    await deskCtx.close();
  } finally {
    await browser.close();
  }

  // --- report what we photographed, for a human eyeball ------------------
  console.log(`candidate name : ${seenName}`);
  console.log(`companies      : ${seenCompanies.map((c) => c.trim()).join(' | ')}`);
  console.log(
    `screenshots    : ${SHOTS}/{mobile,desktop,coverage,pixel,copilot,edit-bar,refine,new-build}.png`,
  );

  // --- fail on console errors -------------------------------------------
  if (allErrors.length) {
    console.error('\nConsole/page errors detected:');
    for (const e of allErrors) console.error('  ' + e);
    process.exit(1);
  }

  // --- fail on any denylisted term in what we rendered -------------------
  // Cover the first page's name/company text AND the full rendered text of
  // every extra page state (pixel, copilot, edit-bar, refine, new-build).
  const haystack =
    `${seenName} ${seenCompanies.join(' ')} ${pageTexts.join(' ')}`.toLowerCase();
  const hits = DENYLIST.filter((term) => haystack.includes(term));
  if (hits.length) {
    console.error(`\nPII LEAK — denylisted term(s) in rendered DOM: ${hits.join(', ')}`);
    process.exit(2);
  }

  console.log('\nOK — no console errors, no denylisted terms.');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
