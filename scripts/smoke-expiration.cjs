// Run against a disposable instance. Tests local/UTC round trips with real
// native forms, DST transitions, fractional offsets, presets, and filtering.
const assert = require('node:assert/strict');
const { randomUUID } = require('node:crypto');
const { chromium } = require('playwright');

async function main() {
  const base = new URL(process.argv[2] || 'http://localhost:8080').origin;
  const browser = await chromium.launch({ headless: true });
  const credentials = process.env.ADMIN_PASSWORD ? {
    httpCredentials: { username: process.env.ADMIN_USERNAME || 'admin', password: process.env.ADMIN_PASSWORD, origin: base },
  } : {};
  try {
    async function api(context, method, path, data) {
      const response = await context.request.fetch(base + '/api/v1/links' + path, {
        method, data, headers: { 'X-Requested-With': 'XMLHttpRequest', 'Content-Type': 'application/json' },
      });
      assert.ok(response.ok(), `${method} ${path}: ${response.status()} ${await response.text()}`);
      return response.status() === 204 ? null : response.json();
    }
    async function submit(page, button, expected = 303) {
      const responsePending = page.waitForResponse(response => response.request().method() === 'POST');
      const loaded = page.waitForEvent('load');
      await button.click();
      const response = await responsePending;
      assert.equal(response.status(), expected, `${response.url()}: native POST failed`);
      await loaded;
    }

    const errors = [];
    for (const [timezoneId, expectedIso] of [
      ['America/Santiago', '2031-06-15T13:15:00.000Z'],
      ['Asia/Kathmandu', '2031-06-15T03:30:00.000Z'],
    ]) {
      const context = await browser.newContext({ ...credentials, timezoneId, locale: 'en-US' });
      const page = await context.newPage();
      page.on('pageerror', error => errors.push(error.message));
      for (const kind of ['url', 'text']) {
        await page.goto(base + '/admin');
        if (kind === 'text') await page.locator('.create-text-heading').click();
        const form = page.locator('form').filter({ has: page.locator(`[name="${kind === 'text' ? 'text_content' : 'target_url'}"]`) });
        const input = form.locator('[data-expiration-input]');
        const hidden = form.locator('[name="expires_at"]');
        assert.match(await form.locator('[data-expiration-label]').textContent(), new RegExp(timezoneId.replace('Kathmandu', 'Kath?mandu')));
        for (const [label, millis] of [['1 hour', 3600000], ['24 hours', 86400000], ['7 days', 604800000]]) {
          const before = Date.now();
          await form.getByRole('button', { name: label, exact: true }).click();
          const expiration = Date.parse(await hidden.inputValue());
          assert.ok(expiration >= before + millis && expiration <= Date.now() + millis, `${label} must start from click time`);
        }
        await form.getByRole('button', { name: 'Never', exact: true }).click();
        assert.equal(await input.inputValue(), '');
        assert.equal(await hidden.inputValue(), '');
        await input.fill('2031-06-15T09:15');
        assert.equal(await hidden.inputValue(), expectedIso);
        const slug = `expiry-${kind}-${randomUUID()}`;
        await form.locator('[name="custom_slug"]').fill(slug);
        await form.locator(`[name="${kind === 'text' ? 'text_content' : 'target_url'}"]`).fill(kind === 'text' ? 'Timezone notes' : 'https://example.com');
        await submit(page, form.getByRole('button', { name: kind === 'text' ? 'Create text link' : 'Create short link', exact: true }));
        assert.equal(Date.parse((await api(context, 'GET', '/' + slug)).expires_at), Date.parse(expectedIso));
        await page.goto(base + '/admin/links/' + slug);
        assert.equal(await page.locator('[data-expiration-input]').inputValue(), '2031-06-15T09:15');
        const display = await page.locator(`time[datetime="${expectedIso.replace('.000Z', 'Z')}"]`).textContent();
        assert.match(display, /9:15:00 AM/);
        await page.locator('[name="label"]').fill('Unrelated edit');
        await submit(page, page.getByRole('button', { name: 'Save changes', exact: true }));
        assert.equal(Date.parse((await api(context, 'GET', '/' + slug)).expires_at), Date.parse(expectedIso));
        // Invalid target validation preserves both the local field and instant.
        if (kind === 'url') {
          await page.locator('[name="target_url"]').fill(base + '/loop');
          await submit(page, page.getByRole('button', { name: 'Save changes', exact: true }), 422);
          assert.equal(await page.locator('[data-expiration-input]').inputValue(), '2031-06-15T09:15');
          assert.equal(await page.locator('[name="expires_at"]').inputValue(), expectedIso);
          await page.locator('[name="target_url"]').fill('https://example.com');
        }
        await page.getByRole('button', { name: 'Never', exact: true }).click();
        await submit(page, page.getByRole('button', { name: 'Save changes', exact: true }));
        assert.equal((await api(context, 'GET', '/' + slug)).expires_at, null);

        // Duplicate create retains the local expiry and source in the draft.
        await page.goto(base + '/admin');
        if (kind === 'text') await page.locator('.create-text-heading').click();
        await form.locator('[name="custom_slug"]').fill(slug);
        await form.locator(`[name="${kind === 'text' ? 'text_content' : 'target_url'}"]`).fill(kind === 'text' ? 'Unsaved notes' : 'https://example.com/draft');
        await input.fill('2031-06-15T09:15');
        await submit(page, form.getByRole('button', { name: kind === 'text' ? 'Create text link' : 'Create short link', exact: true }), 409);
        assert.equal(await input.inputValue(), '2031-06-15T09:15');
        assert.equal(await hidden.inputValue(), expectedIso);
      }
      await context.close();
    }

    const context = await browser.newContext({ ...credentials, timezoneId: 'America/New_York', locale: 'en-US' });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(base + '/admin');
    const localInput = page.locator('#url-expiration');
    await localInput.fill('2030-03-10T02:30');
    assert.equal(await localInput.evaluate(input => input.checkValidity()), false, 'DST gap must not silently shift');
    await localInput.fill('2030-03-10T03:30');
    assert.equal(await localInput.evaluate(input => input.checkValidity()), true);
    assert.equal(await page.locator('#url-expiration').locator('..').locator('[name="expires_at"]').inputValue(), '2030-03-10T07:30:00.000Z');
    let repeated = '2030-11-03T06:30:12.345Z';
    const repeatedSlug = 'fold-' + randomUUID();
    await api(context, 'POST', '', { custom_slug: repeatedSlug, target_url: 'https://example.com', expires_at: repeated });
    await page.goto(base + '/admin/links/' + repeatedSlug);
    assert.equal(await page.locator('[data-expiration-input]').inputValue(), '2030-11-03T01:30:12.345');
    await page.locator('[name="label"]').fill('Preserve later DST occurrence');
    await submit(page, page.getByRole('button', { name: 'Save changes', exact: true }));
    assert.equal((await api(context, 'GET', '/' + repeatedSlug)).expires_at, repeated);
    // An elapsed-hour preset crossing the fall-back transition keeps its instant.
    await page.clock.setFixedTime(new Date('2030-11-03T05:45:12.345Z'));
    repeated = '2030-11-03T06:45:12.345Z';
    await page.getByRole('button', { name: '1 hour', exact: true }).click();
    await submit(page, page.getByRole('button', { name: 'Save changes', exact: true }));
    assert.equal((await api(context, 'GET', '/' + repeatedSlug)).expires_at, repeated);

    const prefix = 'soon-' + randomUUID();
    for (const [suffix, days, text] of [['a', 1, false], ['b', 6, false], ['text', 1, true], ['later', 8, false], ['never', null, false], ['off', 1, false]]) {
      await api(context, 'POST', '', { custom_slug: prefix + '-' + suffix, ...(text ? { text_content: 'soon' } : { target_url: 'https://example.com' }), expires_at: days === null ? null : Date.now() + days * 86400000 });
    }
    await api(context, 'DELETE', '/' + prefix + '-off');
    const query = `?q=${prefix}&status=expiring&kind=url&sort=slug&per_page=1`;
    const listed = await api(context, 'GET', query);
    assert.equal(listed.data.length, 1);
    assert.equal(listed.data[0].slug, prefix + '-a');
    await page.goto(base + '/admin' + query);
    assert.equal(await page.getByLabel('Filter by status').inputValue(), 'expiring');
    assert.equal(await page.locator('tbody tr').count(), 1);
    const next = page.getByRole('link', { name: /Next/ });
    assert.match(await next.getAttribute('href'), /status=expiring&kind=url/);
    await next.click();
    assert.match(await page.locator('tbody').textContent(), new RegExp(prefix + '-b'));
    await submit(page, page.locator('tbody').getByRole('button', { name: 'Disable', exact: true }));
    assert.equal(new URL(page.url()).searchParams.get('status'), 'expiring');
    assert.match(await page.locator('tbody').textContent(), new RegExp(prefix + '-a'));
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(base + '/admin/links/' + repeatedSlug);
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
    if (process.env.SMOKE_SCREENSHOT_DIR) await page.screenshot({ path: process.env.SMOKE_SCREENSHOT_DIR + '/expiration-mobile.png', fullPage: true });

    const noJs = await browser.newContext({ ...credentials, timezoneId: 'Asia/Kathmandu', javaScriptEnabled: false });
    const plain = await noJs.newPage();
    await plain.goto(base + '/admin/links/' + repeatedSlug);
    assert.equal(await plain.locator('[name="expires_at"]').inputValue(), '2030-11-03T06:45:12.345');
    assert.match(await plain.locator('[data-expiration-label]').textContent(), /UTC/);
    await plain.locator('[name="label"]').fill('No JavaScript');
    await submit(plain, plain.getByRole('button', { name: 'Save changes', exact: true }));
    assert.equal((await api(noJs, 'GET', '/' + repeatedSlug)).expires_at, repeated);
    assert.deepEqual(errors, []);
    console.log('Expiration passed: presets for both types, Santiago/Kathmandu conversions, validation drafts, DST gaps/folds, precise saves, filters/pagination, mobile, and no-JS UTC forms');
  } finally {
    await browser.close();
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
