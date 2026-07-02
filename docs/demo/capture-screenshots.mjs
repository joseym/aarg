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

async function main() {
  await mkdir(SHOTS, { recursive: true });
  const browser = await chromium.launch();
  const allErrors = [];
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

    // --- coverage.png : the Coverage-map requirements table -------------
    {
      const page = await deskCtx.newPage();
      const errors = watchErrors(page, 'coverage');
      await page.goto(BASE, { waitUntil: 'networkidle' });
      await waitForPreview(page);
      await page.locator('.segmented button', { hasText: 'Coverage map' }).click();
      await page.waitForSelector('.cov-map .req-row', { timeout: 15_000 });
      await page.waitForTimeout(500);
      // Element screenshot of the coverage map (the requirements table).
      const map = page.locator('.cov-map').first();
      await map.screenshot({ path: `${SHOTS}/coverage.png` });
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
  console.log(`screenshots    : ${SHOTS}/{mobile,desktop,coverage}.png`);

  // --- fail on console errors -------------------------------------------
  if (allErrors.length) {
    console.error('\nConsole/page errors detected:');
    for (const e of allErrors) console.error('  ' + e);
    process.exit(1);
  }

  // --- fail on any denylisted term in what we rendered -------------------
  const haystack = `${seenName} ${seenCompanies.join(' ')}`.toLowerCase();
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
