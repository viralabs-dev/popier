const { chromium } = require('playwright');
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1012, height: 1400 } });
  const body = require('fs').readFileSync('/tmp/claude-0/readme.html','utf8');
  await p.setContent(`<html><head><base href="file:///tmp/claude-0/"><style>body{font-family:-apple-system,Segoe UI,sans-serif;max-width:880px;margin:24px auto;color:#1f2328}img{max-width:100%}code{background:#eff1f3;padding:2px 4px}pre{background:#f6f8fa;padding:12px}</style></head><body>${body}</body></html>`);
  await p.waitForTimeout(1500);
  await p.screenshot({ path: '/tmp/claude-0/readme.png' });
  await b.close();
})();
