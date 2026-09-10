// Run against a disposable instance. Exercises the actual vendored assets and
// native forms, including CSP, hostile Markdown, downloads, and no-JS fallback.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const { randomUUID } = require('node:crypto');
const { chromium } = require('playwright');

async function main() {
  const base = new URL(process.argv[2] || 'http://localhost:8080').origin;
  const browser = await chromium.launch({ headless: true });
  const credentials = process.env.ADMIN_PASSWORD ? {
    httpCredentials: { username: process.env.ADMIN_USERNAME || 'admin', password: process.env.ADMIN_PASSWORD, origin: base },
  } : {};
  try {
    const context = await browser.newContext(credentials);
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.addInitScript(() => {
      window.cspViolations = [];
      document.addEventListener('securitypolicyviolation', event => window.cspViolations.push(event.violatedDirective));
    });
    await page.goto(base + '/admin');
    await page.locator('.create-text-heading').click();
    const editor = page.locator('[data-text-share]');
    const source = editor.locator('textarea');
    const markdown = '# Notes 🦀\n\n**Bold** and `inline`.\n\n```rust\nfn main() { println!("hello"); }\n```\n\n| Feature | Owner | Status | Target | Notes |\n| :--- | ---: | --- | --- | --- |\n| Text shares | Alex | Ready | Sep 12 | Approved |\n';

    async function checkPreviewStyles() {
      // The admin table's mobile column hiding must never discard shared text.
      await page.setViewportSize({ width: 390, height: 844 });
      assert.equal(await page.locator('.text-preview th').count(), 5);
      for (const cell of await page.locator('.text-preview th, .text-preview td').all()) {
        assert.equal(await cell.isVisible(), true, `table cell must remain available: ${await cell.textContent()}`);
      }
      await page.locator('.text-preview th').last().scrollIntoViewIfNeeded();
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true,
        'wide tables must scroll inside the preview, not widen the page');

      await page.setViewportSize({ width: 1280, height: 900 });
      for (const scheme of ['dark', 'light']) {
        await page.emulateMedia({ colorScheme: scheme });
        for (const view of ['Code', 'Markdown preview']) {
          await page.getByRole('button', { name: view, exact: true }).click();
          if (view === 'Code') await page.getByLabel('Code language').selectOption('rust');
          const contrast = await page.locator('.text-preview pre').evaluate(pre => {
            function luminance(color) {
              const channels = color.match(/[\d.]+/g).slice(0, 3).map(value => {
                const channel = Number(value) / 255;
                return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
              });
              return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
            }
            const background = luminance(getComputedStyle(pre).backgroundColor);
            return [...pre.querySelectorAll('code, .hljs-string, .hljs-keyword')].map(element => {
              const foreground = luminance(getComputedStyle(element).color);
              return (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05);
            });
          });
          assert.ok(contrast.length >= 3, 'check source text and highlighted tokens');
          assert.ok(contrast.every(ratio => ratio >= 4.5), `${scheme} ${view}: unreadable code contrast ${contrast}`);
        }
      }
    }
    await source.fill(markdown);
    await editor.getByRole('button', { name: 'Markdown preview', exact: true }).click();
    await editor.locator('.text-preview h1').waitFor();
    assert.equal(await editor.locator('.text-preview strong').textContent(), 'Bold');
    assert.ok(await editor.locator('.text-preview .hljs-keyword').count() > 0);
    assert.equal(await source.isVisible(), true, 'draft remains editable');
    await source.fill(markdown.replace('Notes 🦀', 'Updated draft'));
    await page.waitForFunction(() => document.querySelector('.text-preview h1')?.textContent === 'Updated draft');
    await source.fill(markdown);
    const slug = 'preview-' + randomUUID();
    const form = page.locator('form').filter({ has: source });
    await form.locator('[name="custom_slug"]').fill(slug);
    await Promise.all([
      page.waitForURL(url => url.searchParams.get('created') === slug),
      form.getByRole('button', { name: 'Create text link', exact: true }).click(),
    ]);
    await page.goto(base + '/admin/links/' + slug);
    await page.getByRole('button', { name: 'Markdown preview', exact: true }).click();
    assert.equal(await page.locator('.text-preview h1').textContent(), 'Notes 🦀');
    await checkPreviewStyles();

    await page.goto(base + '/' + slug);
    assert.equal(await page.locator('#shared-text').isVisible(), true, 'public defaults to plain text');
    assert.equal(await page.locator('#shared-text').inputValue(), markdown);
    await page.getByRole('button', { name: 'Markdown preview', exact: true }).click();
    assert.equal(await page.locator('#shared-text').isVisible(), false);
    assert.equal(await page.locator('.text-preview h1').textContent(), 'Notes 🦀');
    assert.ok(await page.locator('.text-preview .hljs-keyword').count() > 0);
    await checkPreviewStyles();
    const downloadPending = page.waitForEvent('download');
    await page.getByRole('link', { name: 'Download as .txt', exact: true }).click();
    const download = await downloadPending;
    assert.equal(download.suggestedFilename(), slug + '.txt');
    // Native URL-encoded forms submit CRLF line endings; downloads preserve
    // those stored bytes, while the browser textarea exposes normalized LF.
    assert.equal(await fs.readFile(await download.path(), 'utf8'), markdown.replace(/\n/g, '\r\n'));
    assert.deepEqual(await page.evaluate(() => window.cspViolations), []);

    // Copy still copies source Markdown, and the manual fallback reveals it.
    await page.evaluate(() => {
      Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async text => { window.copiedSource = text; } } });
    });
    await page.getByRole('button', { name: 'Copy text', exact: true }).click();
    assert.equal(await page.evaluate(() => window.copiedSource), markdown);
    await page.evaluate(() => {
      navigator.clipboard.writeText = async () => { throw new Error('denied'); };
      document.execCommand = () => false;
    });
    await page.getByRole('button', { name: /Copy text|Copied!/, exact: true }).click();
    assert.equal(await page.locator('#shared-text').isVisible(), true);
    assert.match(await page.locator('#copy-status').textContent(), /Text selected/);

    await page.goto(base + '/admin/links/' + slug);
    const hostile = '# Safe\n\n<script>window.previewPwned = true</script>\n\n<img src="/health/live" onerror="window.previewPwned = true">\n\n[bad](javascript:alert(1)) [encoded](jav&#x61;script:alert(1)) [data](data:text/html,evil) [file](file:///etc/passwd)\n\n![tracking](https://tracker.invalid/pixel) ![local](/health/live)\n\n[Good](https://example.com)\n\n```unknown-language\n<img onerror=alert(1)>\n```';
    await page.locator('[name="text_content"]').fill(hostile);
    await Promise.all([
      page.waitForResponse(response => response.request().method() === 'POST' && response.url() === base + '/admin/links/' + slug),
      page.getByRole('button', { name: 'Save changes', exact: true }).click(),
    ]);
    await page.goto(base + '/' + slug);
    const previewRequests = [];
    page.on('request', request => previewRequests.push(request.url()));
    await page.getByRole('button', { name: 'Markdown preview', exact: true }).click();
    const preview = page.locator('.text-preview');
    assert.equal(await preview.locator('script, img, iframe, style, form').count(), 0);
    assert.equal(await preview.locator('a').count(), 1);
    assert.equal(await preview.locator('a').getAttribute('rel'), 'nofollow noopener noreferrer');
    assert.equal(await page.evaluate(() => window.previewPwned), undefined);
    assert.match(await preview.textContent(), /<script>/);
    assert.match(await preview.locator('code').textContent(), /<img onerror=alert\(1\)>/);
    await page.getByRole('button', { name: 'Code', exact: true }).click();
    await page.getByLabel('Code language').selectOption('xml');
    assert.equal(await preview.locator('code').textContent(), hostile);
    assert.ok(await preview.locator('.hljs-tag').count() > 0);
    assert.deepEqual(previewRequests, [], 'switching previews makes no network requests');
    assert.deepEqual(await page.evaluate(() => window.cspViolations), []);
    assert.deepEqual(errors, []);

    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
    if (process.env.SMOKE_SCREENSHOT_DIR) await page.screenshot({ path: process.env.SMOKE_SCREENSHOT_DIR + '/text-share-mobile.png', fullPage: true });
    const noJs = await browser.newContext({ ...credentials, javaScriptEnabled: false });
    const plain = await noJs.newPage();
    await plain.goto(base + '/' + slug);
    assert.equal(await plain.locator('#shared-text').inputValue(), hostile);
    assert.equal(await plain.getByRole('link', { name: 'Download as .txt', exact: true }).isVisible(), true);
    const response = await noJs.request.get(base + '/' + slug + '/download');
    assert.equal(await response.text(), hostile.replace(/\n/g, '\r\n'));
    console.log('Text shares passed: draft/public previews, highlighted code, safe Markdown, exact downloads, copy fallback, mobile, and no-JS access');
  } finally {
    await browser.close();
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
