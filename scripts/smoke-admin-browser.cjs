// Run against a disposable instance: creates, edits, and disables two shares.
// Native form navigation is essential here; fetch/curl with a manually supplied
// Origin does not reproduce Referrer-Policy's effect on browser POST requests.
const assert = require('node:assert/strict');
const { randomUUID } = require('node:crypto');
const { chromium } = require('playwright');

async function main() {
  const base = new URL(process.argv[2] || 'http://localhost:8080').origin;
  const browser = await chromium.launch({ headless: true });
  try {
    const context = await browser.newContext(process.env.ADMIN_PASSWORD ? {
      httpCredentials: {
        username: process.env.ADMIN_USERNAME || 'admin',
        password: process.env.ADMIN_PASSWORD,
        origin: base,
      },
    } : {});
    const page = await context.newPage();
    page.setDefaultTimeout(10_000);

    async function submit(button, path, expected = 303) {
      const pending = page.waitForResponse(response =>
        response.request().method() === 'POST' && response.url() === base + path);
      await button.click();
      const response = await pending;
      assert.equal(response.status(), expected, `${path}: native form POST failed`);
      const headers = await response.request().allHeaders();
      assert.equal(headers.origin, base, 'browser must supply the actual origin');
      assert.ok(headers.referer.startsWith(base + '/'), 'same-origin Referer must remain available');
      await page.waitForLoadState('load');
    }

    for (const kind of ['redirect', 'text']) {
      const slug = `browser-${kind}-${randomUUID()}`;
      await page.goto(base + '/admin');
      const field = kind === 'text' ? 'text_content' : 'target_url';
      if (kind === 'text') await page.locator('summary.create-text-heading').click();
      const form = page.locator('form').filter({ has: page.locator(`[name="${field}"]`) });
      await form.locator(`[name="${field}"]`).fill(
        kind === 'text' ? 'Browser-created notes' : 'https://example.com/browser-created');
      await form.locator('[name="custom_slug"]').fill(slug);
      await submit(form.getByRole('button', { name: kind === 'text' ? 'Create text link' : 'Create short link', exact: true }), '/admin/links');

      const editPath = '/admin/links/' + slug;
      await page.goto(base + editPath);
      await page.locator(`[name="${field}"]`).fill(
        kind === 'text' ? 'Browser-updated notes' : 'https://example.com/browser-updated');
      await submit(page.getByRole('button', { name: 'Save changes', exact: true }), editPath);

      if (kind === 'text') {
        const publicPage = await context.newPage();
        const response = await publicPage.goto(base + '/' + slug);
        assert.equal(response.headers()['referrer-policy'], 'no-referrer');
        assert.equal(await publicPage.locator('#shared-text').inputValue(), 'Browser-updated notes');
        await publicPage.close();
      }
      await submit(page.getByRole('button', { name: 'Disable link', exact: true }), editPath + '/disable');
      await submit(page.getByRole('button', { name: 'Re-enable link', exact: true }), editPath + '/enable');
      await submit(page.getByRole('button', { name: 'Disable link', exact: true }), editPath + '/disable');
    }
    console.log('Browser admin forms passed: create/edit/disable/enable for redirects and shared text');
  } finally {
    await browser.close();
  }
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
