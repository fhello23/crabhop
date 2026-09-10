// Run through Caddy with ADMIN_PASSWORD set. Authenticate only once: persistent
// Playwright httpCredentials would silently sign back in and invalidate the test.
const assert = require('node:assert/strict');
const { chromium } = require('playwright');

async function main() {
  assert.ok(process.env.ADMIN_PASSWORD, 'ADMIN_PASSWORD is required');
  const base = new URL(process.argv[2] || 'http://localhost:8080').origin;
  const browser = await chromium.launch({ headless: true });
  try {
    for (const scenario of [
      'logout-network-error', 'logout-unexpected-status', 'logout-exception',
      'credentials-retained', 'verification-network-error',
      'logout-timeout', 'verification-timeout', 'success',
    ]) {
      const context = await browser.newContext();
      try {
        const page = await context.newPage();
        page.setDefaultTimeout(15_000);
        const errors = [];
        page.on('pageerror', error => errors.push(error.message));
        const cdp = await context.newCDPSession(page);
        let challenges = 0;
        cdp.on('Fetch.requestPaused', event => {
          cdp.send('Fetch.continueRequest', { requestId: event.requestId }).catch(() => {});
        });
        cdp.on('Fetch.authRequired', event => {
          challenges++;
          cdp.send('Fetch.continueWithAuth', {
            requestId: event.requestId,
            authChallengeResponse: challenges === 1 ? {
              response: 'ProvideCredentials',
              username: process.env.ADMIN_USERNAME || 'admin',
              password: process.env.ADMIN_PASSWORD,
            } : { response: 'CancelAuth' },
          }).catch(() => {});
        });
        await cdp.send('Fetch.enable', { handleAuthRequests: true });
        assert.equal((await page.goto(base + '/admin')).status(), 200);
        assert.equal(challenges, 1, 'test must start with an actual authentication challenge');
        await cdp.send('Fetch.disable');
        assert.equal((await page.reload()).status(), 200, 'browser must cache the initial sign-in');

        // Keep another admin tab open to check access independently of the
        // logout page's injected failures and its displayed status message.
        const otherTab = await context.newPage();
        await otherTab.goto(base + '/admin');
        assert.equal((await page.goto(base + '/admin/logout')).status(), 200);
        let injected = false;
        await page.route(url => url.pathname === '/admin', async route => {
          const type = route.request().resourceType();
          if (type === 'xhr' && scenario === 'logout-network-error' ||
              type === 'fetch' && scenario === 'verification-network-error') {
            injected = true;
            return route.abort('failed');
          }
          if (type === 'xhr' && scenario === 'logout-unexpected-status') {
            injected = true;
            return route.fulfill({ status: 200, body: 'Still authenticated' });
          }
          if (type === 'xhr' && scenario === 'credentials-retained') {
            // A rejected bogus credential does not prove that the browser
            // discarded its real credentials. Leave the auth cache intact.
            injected = true;
            return route.fulfill({ status: 401, body: '' });
          }
          if (type === 'xhr' && scenario === 'logout-timeout' ||
              type === 'fetch' && scenario === 'verification-timeout') {
            injected = true;
            return; // Keep the request pending until the client's deadline.
          }
          return route.continue();
        });
        if (scenario === 'logout-exception') {
          injected = true;
          await page.evaluate(() => {
            XMLHttpRequest.prototype.open = () => { throw new Error('Simulated unavailable XHR'); };
          });
        }

        const button = page.getByRole('button', { name: 'Log out', exact: true });
        await button.click();
        if (scenario.endsWith('timeout')) assert.equal(await button.isDisabled(), true);
        await page.waitForFunction(() => {
          const message = document.getElementById('logout-status').textContent;
          return message && message !== 'Logging out…';
        });
        const message = await page.locator('#logout-status').textContent();
        assert.equal(injected, scenario !== 'success', `${scenario}: failure must actually be injected`);
        assert.match(message, scenario === 'success' ? /^Logged out\./ : /^Could not confirm logout\./, scenario);
        assert.equal(await button.isDisabled(), scenario === 'success', `${scenario}: allow retry only after failure`);

        const access = await otherTab.evaluate(async () => {
          const codes = [];
          for (const path of ['/admin', '/api/v1/links']) {
            const response = await fetch(path, { credentials: 'same-origin', cache: 'no-store' });
            codes.push(response.status);
          }
          return codes;
        });
        const cleared = scenario === 'success' || scenario.startsWith('verification-');
        assert.deepEqual(access, cleared ? [401, 401] : [200, 200], `${scenario}: actual protected access`);
        assert.equal(page.url(), base + '/admin/logout', `${scenario}: stay on the result page`);
        assert.deepEqual(errors, [], `${scenario}: no unhandled browser errors`);

        // The original bug left access intact but claimed success. Ensure a
        // transient network failure can be retried without reloading the page.
        if (scenario === 'logout-network-error') {
          await page.unrouteAll();
          await button.click();
          await page.waitForFunction(() => document.getElementById('logout-status').textContent.startsWith('Logged out.'));
          assert.equal(await otherTab.evaluate(async () => (await fetch('/admin', { cache: 'no-store' })).status), 401);
        }
        console.log(`Browser logout passed: ${scenario}`);
      } finally {
        await context.close();
      }
    }
  } finally {
    await browser.close();
  }
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
