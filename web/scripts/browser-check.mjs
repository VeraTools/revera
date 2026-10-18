import { chromium } from 'playwright';
import { parseDocument } from 'yaml';
import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';

const base = process.env.REVERA_WEB_URL || 'http://127.0.0.1:5173';
const emptyYaml = process.argv.includes('--empty-yaml');
const output = join(homedir(), '.local/tmp/revera-server/shots');
await mkdir(output, { recursive: true });
const browser = await chromium.launch({
  executablePath:
    process.env.CHROMIUM_PATH ||
    join(homedir(), '.cache/ms-playwright/chromium-1247/chrome-linux64/chrome'),
  headless: true,
});
const errors = [];
async function open(path, scheme = 'light', viewport = { width: 1440, height: 900 }) {
  const context = await browser.newContext({
    viewport,
    colorScheme: scheme,
    reducedMotion: 'reduce',
  });
  const page = await context.newPage();
  await page.addInitScript(() => {
    // Reuse the app's module instance, including Vite's HMR timestamp. Importing
    // the unversioned URL can create a second mock state and miss observed calls.
    window.importDashboard = (path) => {
      const loaded = performance
        .getEntriesByType('resource')
        .map((entry) => entry.name)
        .findLast((name) => new URL(name).pathname === path);
      return import(loaded || path);
    };
  });
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('console', (message) => {
    if (message.type() === 'error' && message.text().startsWith('Svelte error:'))
      errors.push(message.text());
  });
  await page.goto(`${base}${path}`);
  await page.waitForFunction(() => !document.body.innerText.includes('Loading '));
  return { context, page };
}
async function shot(path, filename, scheme, ready, action) {
  const { context, page } = await open(path, scheme);
  await page.getByRole('heading', { name: ready, exact: true }).waitFor();
  await action?.(page);
  await page.screenshot({ path: join(output, filename), fullPage: path.startsWith('/reviews/') });
  await context.close();
}

const investigator = {
  protocol: 'openai-chat',
  base_url: 'https://api.example.com/v1',
  api_key_env: 'REVERA_MODEL_API_KEY',
  model: 'first-model',
};
function checkMinimal(yaml, differentValidator = false) {
  const doc = parseDocument(yaml);
  assert.deepEqual(doc.errors, [], 'Saved YAML parses');
  assert.deepEqual(
    doc.toJS(),
    {
      models: {
        investigator,
        ...(differentValidator ? { validator: { ...investigator, model: 'different-model' } } : {}),
      },
    },
    'Only the configured model routes and required fields are saved',
  );
}
async function edit(page, id, value) {
  const input = page.locator(`#${id}`);
  await input.fill(value);
  await input.press('Tab');
}
async function firstKey(page) {
  await page.getByRole('button', { name: 'Set key', exact: true }).first().click();
  await page.getByLabel('API key', { exact: true }).fill('fixture-onboarding-key');
  await page.getByRole('dialog').getByRole('button', { name: 'Set key', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  try {
    await page.getByText('Set in dashboard', { exact: true }).first().waitFor();
  } catch (error) {
    console.error('First-key status:', await page.locator('body').innerText());
    throw error;
  }
}

async function checkSetupLink() {
  const { context, page } = await open('/setup?stage=create_app&authorized=0');
  const heading = page.getByRole('heading', { name: 'Open your setup link', exact: true });
  await heading.waitFor();
  assert.equal(await page.locator('form').count(), 0, 'No setup form without cookie or token');
  assert.equal(await page.getByRole('button', { name: 'Create GitHub App' }).count(), 0);
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.getByRole('button', { name: 'Copy command', exact: true }).click();
  assert.equal(
    await page.evaluate(() => navigator.clipboard.readText()),
    'podman logs revera 2>&1 | grep setup',
  );
  await page
    .getByRole('button', { name: 'Copy command', exact: true })
    .getByText('Copied', { exact: true })
    .waitFor();
  await page.getByRole('button', { name: 'Check again', exact: true }).click();
  await heading.waitFor();
  // The site root must return an unauthorized browser to the same guidance.
  await page.locator('.setup-header a').click();
  await page.waitForURL(`${base}/setup`);
  await heading.waitFor();
  assert.equal(await page.locator('form').count(), 0);
  // Simulate the server recognizing the setup cookie in this browser.
  await page.evaluate(async () => {
    const path = '/src/lib/mock.ts';
    const { setMockStage } = await window.importDashboard(path);
    setMockStage('create_app', true);
  });
  await page.getByRole('button', { name: 'Check again', exact: true }).click();
  await page.getByRole('heading', { name: 'Create your GitHub App', exact: true }).waitFor();
  assert.equal(await page.locator('form').count(), 1, 'Check again reloads authorization');
  await context.close();

  for (const [query, title] of [
    ['stage=create_app&authorized=1', 'Create your GitHub App'],
    ['stage=create_app&authorized=0&token=fixture', 'Create your GitHub App'],
    ['stage=sign_in&authorized=0', 'Sign in with GitHub'],
  ]) {
    const { context, page } = await open(`/setup?${query}`);
    await page.getByRole('heading', { name: title, exact: true }).waitFor();
    assert.equal(
      await page.getByRole('heading', { name: 'Open your setup link', exact: true }).count(),
      0,
    );
    await context.close();
  }
}

async function choose(page, label, option) {
  await page.getByLabel(label, { exact: true }).click();
  await page.getByRole('option', { name: option, exact: true }).click();
  await page.locator('.select-content').waitFor({ state: 'hidden' });
}

async function savedConfig(page) {
  const yaml = await page.evaluate(async () => {
    const path = '/src/lib/api.ts';
    const { api } = await window.importDashboard(path);
    return (await api.settings()).yaml;
  });
  const doc = parseDocument(yaml);
  assert.deepEqual(doc.errors, [], 'Wizard saves valid YAML');
  return doc;
}

async function testWizard(page) {
  // Observe calls without replacing the mock's real save/test behavior.
  await page.evaluate(async () => {
    const path = '/src/lib/api.ts';
    const { api } = await window.importDashboard(path);
    if (!window.testedRoles) {
      const test = api.test;
      api.test = (role) => {
        window.testedRoles.push(role);
        return test(role);
      };
    }
    window.testedRoles = [];
  });
  await page.getByRole('button', { name: 'Test connections', exact: true }).click();
  await page.getByRole('button', { name: 'Finish setup', exact: true }).waitFor();
  await page.waitForFunction(() =>
    [...document.querySelectorAll('button')].some(
      (button) => button.textContent === 'Finish setup' && !button.disabled,
    ),
  );
  assert.deepEqual(await page.evaluate(() => window.testedRoles), [
    'investigator',
    'validator',
    'embedding',
  ]);
  return savedConfig(page);
}

async function checkWizardReranker() {
  {
    const { context, page } = await open('/setup?stage=configure');
    await page.getByRole('heading', { name: 'Connect your review models', exact: true }).waitFor();
    if (emptyYaml) {
      await edit(page, 'model-investigator-endpoint', investigator.base_url);
      await edit(page, 'model-investigator-name', investigator.model);
      await firstKey(page);
    }
    await page.locator('summary').filter({ hasText: 'Repository search' }).click();
    const retrieval = page.getByLabel('Enable Vera retrieval', { exact: true });
    if ((await retrieval.getAttribute('aria-checked')) === 'false') await retrieval.click();
    await choose(page, 'Embedding backend', 'API endpoint');
    const reranker = page.getByLabel('Enable reranker', { exact: true });
    assert.equal(await reranker.getAttribute('aria-checked'), 'true', 'Reranker defaults on');
    assert.equal(await page.locator('#model-reranker-name').inputValue(), '');
    assert.equal(await page.locator('.reranker-fields input').count(), 1, 'Only a model is needed');
    await edit(page, 'model-embedding-endpoint', 'https://api.example.com/v1');
    await edit(page, 'model-embedding-name', 'embedding-model');
    await edit(page, 'model-embedding-env', 'EMBEDDING_KEY');
    const embeddingKey = page
      .locator('.field')
      .filter({ has: page.locator('#model-embedding-env') });
    await embeddingKey.getByRole('button', { name: 'Set key', exact: true }).click();
    await page.getByLabel('API key', { exact: true }).fill('fixture-embedding-key');
    await page.getByRole('dialog').getByRole('button', { name: 'Set key', exact: true }).click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    await edit(page, 'model-reranker-name', 'reranker-model');
    await choose(page, 'Embedding backend', 'Local (on-device)');
    assert.equal(await page.locator('.reranker-fields').count(), 0);
    await choose(page, 'Embedding backend', 'API endpoint');
    await retrieval.click();
    await retrieval.click();
    assert.equal(await reranker.getAttribute('aria-checked'), 'true');
    assert.equal(await page.locator('#model-reranker-name').inputValue(), 'reranker-model');
    const doc = await testWizard(page);
    const expected = {
      base_url: 'https://api.example.com/v1',
      model: 'reranker-model',
      api_key_env: 'EMBEDDING_KEY',
    };
    assert.deepEqual(doc.getIn(['vera', 'reranker']).toJSON(), expected);
    assert.equal((await page.locator('body').innerText()).includes('fixture-embedding-key'), false);
    await edit(page, 'model-reranker-name', 'changed-reranker');
    assert.equal(await page.getByRole('button', { name: 'Finish setup' }).isEnabled(), false);
    await page.getByRole('button', { name: 'Test connections', exact: true }).click();
    await page.getByRole('button', { name: 'Finish setup', exact: true }).click();
    await page.getByRole('link', { name: 'Settings', exact: true }).click();
    await page.getByRole('tab', { name: 'Retrieval', exact: true }).click();
    assert.equal(await page.locator('#model-reranker-endpoint').inputValue(), expected.base_url);
    assert.equal(await page.locator('#model-reranker-env').inputValue(), expected.api_key_env);
    assert.equal(await page.locator('#model-reranker-name').inputValue(), 'changed-reranker');
    await choose(page, 'Reranker format', 'Top-k format');
    await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
    const settings = parseDocument(await page.getByLabel('Review configuration YAML').inputValue());
    assert.deepEqual(settings.getIn(['vera', 'reranker']).toJSON(), {
      ...expected,
      model: 'changed-reranker',
      protocol: 'voyage',
    });
    await context.close();
  }

  // Resume with an API embedding route, with and without an existing reranker.
  for (const custom of [false, true]) {
    const { context, page } = await open('/setup?stage=install');
    await page.getByRole('button', { name: 'Install on repositories', exact: true }).waitFor();
    const reranker = {
      base_url: 'https://reranker.example.com/v1',
      api_key_env: 'RERANKER_KEY',
      model: 'saved-reranker',
      protocol: 'generic',
      top_k: 12,
    };
    const yaml = `# Keep this setup comment
models:
  investigator: ${JSON.stringify(investigator)}
vera:
  enabled: true
  backend: api
  embedding:
    base_url: https://api.example.com/v1
    api_key_env: EMBEDDING_KEY
    model: embedding-model
${custom ? `  reranker: ${JSON.stringify(reranker)}\n` : ''}`;
    await page.evaluate(async (yaml) => {
      const path = '/src/lib/api.ts';
      const { api } = await window.importDashboard(path);
      await api.saveConfig(yaml);
    }, yaml);
    await page.getByRole('button', { name: 'Install on repositories', exact: true }).click();
    await page.locator('summary').filter({ hasText: 'Repository search' }).click();
    const toggle = page.getByLabel('Enable reranker', { exact: true });
    assert.equal(await toggle.getAttribute('aria-checked'), 'true');
    assert.equal(
      await page.locator('#model-reranker-name').inputValue(),
      custom ? reranker.model : '',
    );
    await edit(page, 'model-embedding-endpoint', 'https://embedding.example.com/v1');
    await edit(page, 'model-embedding-env', 'UPDATED_EMBEDDING_KEY');
    if (custom) {
      const doc = await testWizard(page);
      assert.deepEqual(doc.getIn(['vera', 'reranker']).toJSON(), reranker);
      assert.ok(doc.toString().includes('# Keep this setup comment'));
    } else {
      await toggle.click();
      await choose(page, 'Embedding backend', 'Local (on-device)');
      await choose(page, 'Embedding backend', 'API endpoint');
      await page.getByLabel('Enable Vera retrieval', { exact: true }).click();
      await page.getByLabel('Enable Vera retrieval', { exact: true }).click();
      assert.equal(await toggle.getAttribute('aria-checked'), 'false', 'Off choice survives');
      assert.equal((await testWizard(page)).hasIn(['vera', 'reranker']), false);
      await toggle.click();
      await edit(page, 'model-reranker-name', 'new-reranker');
      const doc = await testWizard(page);
      assert.deepEqual(doc.getIn(['vera', 'reranker']).toJSON(), {
        base_url: 'https://embedding.example.com/v1',
        api_key_env: 'UPDATED_EMBEDDING_KEY',
        model: 'new-reranker',
      });
    }
    await context.close();
  }
}

async function checkReviewSettings() {
  const { context, page } = await open('/settings?tab=review');
  await page.getByLabel('Default trigger', { exact: true }).waitFor();
  await choose(page, 'Default trigger', 'Auto');
  await choose(page, 'Default mode', 'Deep');
  await page.getByText('Mention commands', { exact: true }).click();
  await page.getByText('@revera-northstar review', { exact: true }).waitFor();
  await page.getByText('@revera-northstar help', { exact: true }).waitFor();
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await page.getByText('Settings saved.', { exact: true }).waitFor();
  const settings = await page.evaluate(async () => {
    const { api } = await window.importDashboard('/src/lib/api.ts');
    return api.serverSettings();
  });
  assert.equal(settings.default_trigger, 'auto');
  assert.equal(settings.default_mode, 'deep');
  assert.equal('default_enabled' in settings, false);
  await page.getByRole('link', { name: 'Repositories', exact: true }).click();
  assert.equal(
    await page.getByLabel('Trigger for northstar/api', { exact: true }).innerText(),
    'Default (Auto)',
  );
  assert.equal(
    await page.getByLabel('Mode for northstar/api', { exact: true }).innerText(),
    'Default (Deep)',
  );
  await choose(page, 'Trigger for northstar/api', 'Off');
  await page.getByText('northstar/api: trigger saved.', { exact: true }).waitFor();
  await page.getByRole('button', { name: 'Actions for northstar/api', exact: true }).click();
  assert.equal(
    await page
      .getByRole('menuitem', { name: 'Review a pull request' })
      .getAttribute('data-disabled'),
    '',
  );
  await page.keyboard.press('Escape');
  await choose(page, 'Trigger for northstar/api', 'Default (Auto)');
  await choose(page, 'Mode for northstar/api', 'Quick');
  await page.getByText('northstar/api: mode saved.', { exact: true }).waitFor();
  await choose(page, 'Mode for northstar/api', 'Default (Deep)');
  await page.getByLabel('Search repositories', { exact: true }).fill('worker');
  assert.equal(await page.locator('tbody tr').count(), 1);
  await page.getByLabel('Search repositories', { exact: true }).fill('');
  await page
    .getByRole('group', { name: 'Filter by trigger' })
    .getByRole('button', { name: /^Off / })
    .click();
  assert.equal(await page.locator('tbody tr').count(), 2);
  await page
    .getByRole('group', { name: 'Filter by trigger' })
    .getByRole('button', { name: /^All / })
    .click();
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    true,
    'Repository controls and the scrollable table stay within the mobile page',
  );
  await context.close();
}

async function checkFallbacksAndWebSearch() {
  const { context, page } = await open('/settings?tab=advanced');
  const editor = page.getByLabel('Review configuration YAML');
  await editor.waitFor();
  await editor.fill(
    `# Preserve route comments
models:
  investigator:
    protocol: openai-chat
    base_url: https://api.example.com/v1
    api_key_env: REVERA_MODEL_API_KEY
    model: primary-model
    fallback:
      # Keep the first alternative
      - protocol: openai-chat
        base_url: https://backup.example.com/v1
        api_key_env: REVERA_BACKUP_API_KEY
        model: first-backup
        extra_headers: {x-team: northstar}
      - protocol: openai-chat
        base_url: https://backup.example.com/v1
        api_key_env: REVERA_BACKUP_API_KEY
        model: second-backup
web_search:
  enabled: false
  max_searches_per_review: 7 # Keep this limit
`,
  );
  await page.getByRole('tab', { name: 'Models', exact: true }).click();
  const card = page.locator('section[aria-label="investigator"]');
  const validator = page.locator('section[aria-label="validator"]');
  await validator.getByLabel('Use a different validator model', { exact: true }).click();
  await card.getByText('Fallback models (2)', { exact: true }).click();
  await validator.getByText('Fallback models (2)', { exact: true }).click();
  assert.equal(await page.locator('#model-validator-fallback-0-name').inputValue(), 'first-backup');
  await card.getByLabel('Move fallback 2 up', { exact: true }).click();
  assert.equal(
    await page.locator('#model-investigator-fallback-0-name').inputValue(),
    'second-backup',
  );
  await validator.getByLabel('Remove fallback 2', { exact: true }).click();
  await validator.getByLabel('Remove fallback 1', { exact: true }).click();
  assert.equal(
    await validator.locator('.fallback-route').count(),
    0,
    'Explicit validator has no inherited alternatives',
  );
  await card.getByRole('button', { name: 'Add fallback', exact: true }).click();
  await edit(page, 'model-investigator-fallback-2-name', 'third-backup');
  await card.getByLabel('Remove fallback 1', { exact: true }).click();
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await page.getByText('Settings saved.', { exact: true }).waitFor();
  await page.evaluate(async () => {
    const { api } = await window.importDashboard('/src/lib/api.ts');
    const test = api.test;
    window.fallbackCalls = [];
    api.test = (role, fallback) => {
      window.fallbackCalls.push({ role, fallback });
      return test(role, fallback);
    };
  });
  await card.getByLabel('Test investigator fallback 1', { exact: true }).click();
  await card.getByText('342 ms', { exact: true }).waitFor();
  assert.deepEqual(await page.evaluate(() => window.fallbackCalls), [
    { role: 'investigator', fallback: 0 },
  ]);
  await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
  let doc = parseDocument(await editor.inputValue());
  assert.deepEqual(
    doc
      .getIn(['models', 'investigator', 'fallback'])
      .toJSON()
      .map((route) => route.model),
    ['first-backup', 'third-backup'],
  );
  assert.deepEqual(doc.getIn(['models', 'investigator', 'fallback', 0, 'extra_headers']).toJSON(), {
    'x-team': 'northstar',
  });
  assert.ok(
    (await editor.inputValue()).includes('# Keep the first alternative'),
    'Comments survive moving and cloning routes',
  );
  assert.equal(doc.hasIn(['models', 'validator', 'fallback']), false);
  await page.getByRole('tab', { name: 'Retrieval', exact: true }).click();
  await page.getByLabel('Enable web search', { exact: true }).click();
  assert.equal(await page.getByLabel('Use the free endpoint first', { exact: true }).count(), 0);
  await page.getByRole('button', { name: 'Add key', exact: true }).click();
  await page.getByLabel('API key', { exact: true }).fill('fixture-exa-key');
  await page.getByRole('dialog').getByRole('button', { name: 'Set key', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  await page.getByLabel('Use the free endpoint first', { exact: true }).click();
  await page.getByLabel('Allow on private repositories', { exact: true }).click();
  await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
  doc = parseDocument(await editor.inputValue());
  assert.deepEqual(doc.getIn(['web_search']).toJSON(), {
    enabled: true,
    max_searches_per_review: 7,
    api_key_env: 'EXA_API_KEY',
    free_first: true,
    allow_private: true,
  });
  assert.ok((await editor.inputValue()).includes('# Keep this limit'));
  assert.equal((await page.locator('body').innerText()).includes('fixture-exa-key'), false);
  await context.close();
}

async function checkMentionsAndReviewDetail() {
  const { context, page } = await open('/');
  const notice = page.getByRole('heading', {
    name: 'Mentions need one change on your GitHub App',
    exact: true,
  });
  await notice.waitFor();
  assert.ok(
    (
      await page.getByRole('link', { name: 'Open GitHub App settings' }).getAttribute('href')
    ).endsWith('/permissions'),
  );
  for (const subscribed of [true, null]) {
    await page.evaluate(async (value) => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const overview = await api.overview();
      overview.app.issue_comment_subscribed = value;
      api.overview = async () => overview;
    }, subscribed);
    await page.getByRole('link', { name: 'Reviews', exact: true }).click();
    await page.getByRole('link', { name: 'Overview', exact: true }).click();
    await page.getByText('Everything is connected', { exact: true }).waitFor();
    assert.equal(await notice.count(), 0);
  }
  await page.getByRole('link', { name: 'Reviews', exact: true }).click();
  await page
    .getByRole('link', { name: 'Handle empty results in repository search', exact: true })
    .click();
  await page
    .getByText(
      'Investigator switched from model-large to model-small: rate limit or server error.',
      { exact: true },
    )
    .waitFor();
  await page
    .getByText('Validator skipped model-alternative: key not set.', { exact: true })
    .waitFor();
  assert.equal(await page.getByLabel('Review mode', { exact: true }).innerText(), 'Default (Deep)');
  await choose(page, 'Review mode', 'Quick');
  await page.getByRole('button', { name: 'Re-run review', exact: true }).click();
  await page.getByText('A new review has been queued.', { exact: true }).waitFor();
  const jobs = await page.evaluate(async () => {
    const { api } = await window.importDashboard('/src/lib/api.ts');
    return api.jobs();
  });
  assert.equal(jobs.jobs[0].mode, 'quick');
  assert.equal(jobs.jobs[0].trigger, 'rerun');
  await context.close();
}

async function checkThemeAndRetrievalLink() {
  const { context, page } = await open('/', 'light');
  await page.getByRole('heading', { name: 'Overview', exact: true }).waitFor();
  await page.getByRole('button', { name: 'Theme', exact: true }).click();
  await page.getByRole('menuitemradio', { name: 'Dark', exact: true }).click();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), 'dark');
  assert.equal(
    await page.evaluate(() => {
      // Resolve --bg in a color property: custom properties retain light-dark() as text.
      const probe = document.createElement('div');
      probe.style.backgroundColor = 'var(--bg)';
      document.body.append(probe);
      const background = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return background;
    }),
    'rgb(24, 24, 27)',
    'Dark choice overrides the light system scheme',
  );
  await page.reload();
  await page.getByRole('heading', { name: 'Overview', exact: true }).waitFor();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), 'dark');
  await page.getByRole('button', { name: 'Theme', exact: true }).click();
  assert.equal(
    await page
      .getByRole('menuitemradio', { name: 'Dark', exact: true })
      .getAttribute('aria-checked'),
    'true',
    'Theme menu reflects the persisted choice',
  );
  await page.getByRole('menuitemradio', { name: 'System', exact: true }).click();
  assert.equal(
    await page.evaluate(() => document.documentElement.hasAttribute('data-theme')),
    false,
  );
  assert.equal(await page.evaluate(() => localStorage.getItem('revera-theme')), null);
  await page.goto(`${base}/settings`);
  await page.getByRole('button', { name: 'Retrieval tab', exact: true }).click();
  await page.getByRole('heading', { name: 'Repository retrieval', exact: true }).waitFor();
  await context.close();
}

async function checkAuditSettings() {
  const { context, page } = await open('/settings');
  await page.getByRole('heading', { name: 'Review models', exact: true }).waitFor();
  const original = await savedConfig(page);
  const investigator = page.locator('section[aria-label="investigator"]');
  const investigatorKey = investigator.locator('.field').filter({
    has: page.locator('#model-investigator-env'),
  });
  const validator = page.locator('section[aria-label="validator"]');
  const keyName = original.getIn(['models', 'investigator', 'api_key_env']);
  const referenced = page.locator('.secret-row').filter({ hasText: keyName });
  const override = 'Set in dashboard (overrides environment)';
  await investigatorKey.getByText(override, { exact: true }).waitFor();
  await referenced.getByText(override, { exact: true }).waitFor();
  await page.screenshot({ path: join(output, 'settings-key-override-light.png'), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(
    await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    true,
    'Key override status and removal controls fit on mobile',
  );
  await page.setViewportSize({ width: 1440, height: 900 });
  page.on('dialog', (dialog) => dialog.accept());
  await investigatorKey.getByRole('button', { name: 'Remove', exact: true }).click();
  await investigatorKey.getByText('Set in environment', { exact: true }).waitFor();
  await referenced.getByText('Set in environment', { exact: true }).waitFor();
  await referenced.getByRole('button', { name: 'Set key', exact: true }).click();
  await page.getByLabel('API key', { exact: true }).fill('fixture-audit-override-key');
  await page.getByRole('dialog').getByRole('button', { name: 'Set key', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  await referenced.getByText(override, { exact: true }).waitFor();
  await referenced.getByRole('button', { name: 'Remove', exact: true }).click();
  await referenced.getByText('Set in environment', { exact: true }).waitFor();

  const separate = validator.getByLabel('Use a different validator model', { exact: true });
  assert.equal(await separate.getAttribute('aria-checked'), 'true');
  await separate.click();
  assert.equal(await page.locator('#model-validator-name').count(), 0);
  await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
  const editor = page.getByLabel('Review configuration YAML');
  let doc = parseDocument(await editor.inputValue());
  assert.equal(doc.hasIn(['models', 'validator']), false, 'Off removes the validator override');
  assert.deepEqual(
    doc.getIn(['models', 'investigator']).toJSON(),
    original.getIn(['models', 'investigator']).toJSON(),
    'Returning to inheritance leaves the investigator unchanged',
  );
  await page.getByRole('tab', { name: 'Models', exact: true }).click();
  assert.equal(await separate.getAttribute('aria-checked'), 'false');
  await page.screenshot({
    path: join(output, 'settings-models-inherited-light.png'),
    fullPage: true,
  });
  await separate.click();
  await edit(page, 'model-validator-name', 'audit-validator');
  await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
  doc = parseDocument(await editor.inputValue());
  assert.equal(doc.getIn(['models', 'validator', 'model']), 'audit-validator');
  await page.getByRole('tab', { name: 'Models', exact: true }).click();
  assert.equal(await separate.getAttribute('aria-checked'), 'true');
  await separate.click();

  await page.getByRole('tab', { name: 'Review', exact: true }).click();
  assert.equal(await page.getByLabel('Publish mode', { exact: true }).count(), 0);
  assert.equal(await page.locator('#review-publish').count(), 0);
  const forks = page.getByLabel('Review pull requests from forks', { exact: true });
  assert.equal(await forks.getAttribute('aria-checked'), 'false', 'Fork reviews default off');
  await forks.click();
  await page.getByRole('tab', { name: 'Budgets', exact: true }).click();
  assert.equal(await page.getByLabel('Largest diff', { exact: true }).inputValue(), '200000');
  await edit(page, 'review-max-diff-bytes', '250000');
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await page.getByText('Settings saved.', { exact: true }).waitFor();
  doc = await savedConfig(page);
  assert.equal(doc.getIn(['github', 'allow_forks']), true);
  assert.equal(doc.getIn(['review', 'max_diff_bytes']), 250000);
  assert.equal(doc.hasIn(['models', 'validator']), false);
  await page.getByRole('tab', { name: 'Review', exact: true }).click();
  assert.equal(await forks.getAttribute('aria-checked'), 'true');
  await forks.click();
  await page.getByRole('button', { name: 'Save changes', exact: true }).click();
  await page.getByText('Settings saved.', { exact: true }).waitFor();
  assert.equal((await savedConfig(page)).getIn(['github', 'allow_forks']), false);

  await page.getByRole('tab', { name: 'Retrieval', exact: true }).click();
  assert.equal(
    await page.getByLabel('Key environment variable (optional)').inputValue(),
    'EXA_API_KEY',
  );
  await page.getByText('Using the free endpoint', { exact: true }).waitFor();
  await edit(page, 'web-search-env', keyName);
  const webSearch = page.locator('.web-search');
  await webSearch.getByText('Set in environment', { exact: true }).waitFor();
  await webSearch.getByRole('button', { name: 'Replace key', exact: true }).click();
  await page.getByLabel('API key', { exact: true }).fill('fixture-audit-search-key');
  await page.getByRole('dialog').getByRole('button', { name: 'Set key', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  await webSearch.getByText(override, { exact: true }).waitFor();
  await webSearch.getByRole('button', { name: 'Remove', exact: true }).click();
  await webSearch.getByText('Set in environment', { exact: true }).waitFor();
  assert.equal((await page.locator('body').innerText()).includes('fixture-audit-'), false);
  await context.close();
}

async function checkDashboardQol() {
  const { context, page } = await open('/repositories');
  await page.getByRole('heading', { name: 'Repositories', exact: true }).waitFor();
  await page.locator('tbody tr').first().waitFor();
  const names = () => page.locator('tbody .row-title').allTextContents();
  const recentOrder = await names();
  assert.equal(recentOrder.length, 12);
  await choose(page, 'Sort repositories', 'Sort: Name');
  const nameOrder = await names();
  assert.notDeepEqual(nameOrder, recentOrder, 'Name sort changes row order');
  assert.deepEqual(
    nameOrder,
    [...nameOrder].sort((a, b) => a.localeCompare(b)),
  );
  assert.equal(new URL(page.url()).searchParams.get('sort'), 'name');
  const segments = page.getByRole('group', { name: 'Filter by trigger' });
  await segments.getByRole('button', { name: /^Auto / }).click();
  assert.equal(await page.locator('tbody tr').count(), 3);
  assert.equal(new URL(page.url()).searchParams.get('trigger'), 'auto');
  await page.reload();
  await page.getByLabel('Sort repositories', { exact: true }).waitFor();
  assert.equal(
    await page.getByLabel('Sort repositories', { exact: true }).innerText(),
    'Sort: Name',
  );
  assert.equal(
    await segments.getByRole('button', { name: /^Auto / }).getAttribute('aria-pressed'),
    'true',
  );
  await segments.getByRole('button', { name: /^All / }).click();
  await page.getByLabel('Select northstar/api', { exact: true }).check();
  await page.getByLabel('Select northstar/billing', { exact: true }).check();
  assert.equal(
    await page
      .getByLabel('Select all visible repositories', { exact: true })
      .evaluate((el) => el.indeterminate),
    true,
  );
  await choose(page, 'Set trigger for selected repositories', 'Auto');
  await page.getByText('Trigger set to Auto for 2 repositories.', { exact: true }).waitFor();
  for (const name of ['api', 'billing'])
    assert.equal(
      await page.getByLabel(`Trigger for northstar/${name}`, { exact: true }).innerText(),
      'Auto',
    );
  assert.equal(
    await page.getByLabel('Set trigger for selected repositories', { exact: true }).innerText(),
    'Set trigger…',
  );
  await choose(page, 'Set mode for selected repositories', 'Quick');
  await page.getByText('Mode set to Quick for 2 repositories.', { exact: true }).waitFor();
  for (const name of ['api', 'billing'])
    assert.equal(
      await page.getByLabel(`Mode for northstar/${name}`, { exact: true }).innerText(),
      'Quick',
    );
  await page.getByLabel('Search repositories', { exact: true }).fill('api');
  await page.getByText('1 selected', { exact: true }).waitFor();
  assert.equal(new URL(page.url()).searchParams.get('q'), 'api');
  await page.getByLabel('Search repositories', { exact: true }).fill('');
  await page.getByLabel('Select all visible repositories', { exact: true }).check();
  await page.getByText('12 selected', { exact: true }).waitFor();
  await page
    .getByLabel('Bulk repository actions')
    .getByRole('button', { name: 'Index now', exact: true })
    .click();
  await page
    .getByText(
      'Index queued for 9 repositories. 2 skipped (reviews off). 1 skipped (index building).',
      { exact: true },
    )
    .waitFor();
  await choose(page, 'Activity', 'No activity in 30 days');
  assert.deepEqual(await names(), ['northstar/docs', 'northstar/notifications', 'northstar/sdk']);
  await page.getByRole('link', { name: 'Reviews', exact: true }).click();
  await page.getByLabel('Search reviews', { exact: true }).fill('empty results');
  await page.waitForFunction(
    () => document.querySelector('.result-count')?.textContent === '1 job',
  );
  assert.equal(new URL(page.url()).searchParams.get('q'), 'empty results');
  await page.getByLabel('Search reviews', { exact: true }).fill('#234');
  await page.waitForFunction(
    () => document.querySelector('.result-count')?.textContent === '1 job',
  );
  await page
    .getByRole('link', { name: 'Handle empty results in repository search', exact: true })
    .click();
  await page.locator('.summary ul').first().waitFor();
  assert.equal((await page.locator('.summary').innerText()).includes('##'), false);
  assert.ok(await page.locator('.summary blockquote').count());
  assert.ok(await page.locator('.summary code').count());
  assert.equal(await page.locator('.summary-footer').count(), 1);
  assert.ok(
    (
      await page
        .getByRole('link', { name: 'src/search/results.ts:84', exact: true })
        .getAttribute('href')
    ).endsWith('/blob/8c74a1e9bf062d4a18f60c32/src/search/results.ts#L84'),
  );
  await page.getByRole('heading', { name: 'Model usage', exact: true }).waitFor();
  await page.getByText('Raw report data', { exact: true }).click();
  assert.ok((await page.locator('.data-disclosure pre').innerText()).includes('"stats"'));
  const rendered = await page.evaluate(async () => {
    const { renderSummary } = await import('/src/lib/markdown.ts');
    return {
      xss: renderSummary('<img src=x onerror=alert(1)> & " \' <script>alert(1)</script>'),
      links: renderSummary('[safe](https://example.com/a?q=1&x=2) [unsafe](javascript:alert(1))'),
      inline: renderSummary('`**literal**` **bold** _italic_ file_name_unchanged'),
      example: renderSummary(
        '## Revera review\n\n**Status: complete** — 1 new finding\n\n- **[medium]** `slop/compare.go`:146 — Negative deltas\n\n> Review incomplete: investigator reported 1 unchecked area(s).\n\n### Resolved since last review\n\n- shortSHA panics on abbreviated commits\n\n_Timing: total 75s_\n\n<sub>revera · strategy baseline</sub>',
      ),
    };
  });
  assert.equal(rendered.xss.includes('<img'), false);
  assert.equal(rendered.xss.includes('<script'), false);
  for (const entity of ['&lt;', '&gt;', '&amp;', '&quot;', '&#39;'])
    assert.ok(rendered.xss.includes(entity));
  assert.equal((rendered.links.match(/<a /g) || []).length, 1);
  assert.ok(rendered.links.includes('rel="noreferrer" target="_blank"'));
  assert.ok(rendered.inline.includes('<code>**literal**</code>'));
  assert.ok(rendered.inline.includes('<strong>bold</strong>'));
  assert.ok(rendered.inline.includes('<em>italic</em>'));
  assert.ok(rendered.inline.includes('file_name_unchanged'));
  for (const tag of ['<ul>', '<blockquote>', '<code>', '<h4>', 'summary-footer'])
    assert.ok(rendered.example.includes(tag));
  assert.equal(rendered.example.includes('##'), false);
  await page.goto(`${base}/reviews?repo=northstar%2Fapi&status=complete&kind=review&q=%23227`);
  await page.waitForFunction(
    () => document.querySelector('.result-count')?.textContent === '1 job',
  );
  assert.equal(await page.getByLabel('Search reviews', { exact: true }).inputValue(), '#227');
  assert.equal(await page.getByLabel('Repository', { exact: true }).innerText(), 'northstar/api');
  assert.equal(await page.getByLabel('Status', { exact: true }).innerText(), 'Complete');
  assert.equal(await page.getByLabel('Job kind', { exact: true }).innerText(), 'Reviews only');
  await page.getByRole('link', { name: 'Repositories', exact: true }).click();
  await page.getByRole('heading', { name: 'Repositories', exact: true }).waitFor();
  await page.goBack();
  await page.waitForFunction(
    () => document.querySelector('.result-count')?.textContent === '1 job',
  );
  assert.equal(await page.getByLabel('Search reviews', { exact: true }).inputValue(), '#227');
  await context.close();

  const overview = await open('/');
  await overview.page
    .getByRole('group', { name: 'Overview period' })
    .getByRole('button', { name: '7 days', exact: true })
    .click();
  await overview.page.getByText(/History keeps\s+the\s+last 500 jobs/).waitFor();
  await overview.page.reload();
  assert.equal(
    await overview.page
      .getByRole('group', { name: 'Overview period' })
      .getByRole('button', { name: '7 days', exact: true })
      .getAttribute('aria-pressed'),
    'true',
  );
  await overview.page.getByRole('button', { name: 'Collapse navigation', exact: true }).click();
  await overview.page.reload();
  await overview.page.getByRole('button', { name: 'Expand navigation', exact: true }).waitFor();
  await overview.context.close();

  const settings = await open('/settings?tab=budgets');
  await settings.page
    .getByText('Quick: 12 tool calls and 2.5 min per agent, 7.5 min per review.', { exact: true })
    .waitFor();
  await settings.page
    .getByText(
      'Deep: 50 tool calls and 10 min per agent, 30 min and 240 requests per review, diffs up to 400 KB, up to 20 findings.',
      { exact: true },
    )
    .waitFor();
  await edit(settings.page, 'budget-agent_max_tool_calls', '40');
  await settings.page
    .getByText('Quick: 20 tool calls and 2.5 min per agent, 7.5 min per review.', { exact: true })
    .waitFor();
  const profiles = await settings.page.evaluate(async () => {
    const { quickBudget, deepBudget } = await import('/src/lib/profiles.ts');
    const b = {
      agent_max_tool_calls: 1,
      agent_max_seconds: 20,
      run_max_seconds: 40,
      run_max_requests: 120,
      retries: 3,
    };
    return {
      quick: quickBudget(b),
      deep: deepBudget(
        { ...b, agent_max_seconds: 4000, run_max_seconds: 2000 },
        { max_diff_bytes: 500000, max_findings: 25 },
      ),
    };
  });
  assert.equal(profiles.quick.agent_max_tool_calls, 1);
  assert.equal(profiles.quick.agent_max_seconds, 30);
  assert.equal(profiles.quick.run_max_seconds, 60);
  assert.equal(profiles.deep.agent_max_seconds, 4000);
  assert.equal(profiles.deep.run_max_seconds, 3000);
  assert.equal(profiles.deep.run_max_requests, 240);
  assert.equal(profiles.deep.review.max_diff_bytes, 500000);
  assert.equal(profiles.deep.review.max_findings, 25);
  await settings.context.close();
}

// Lane B: URL review, private-source badge and explicit tracking chooser.
async function checkExternalReviews() {
  const { context, page } = await open('/reviews');
  await page.getByRole('button', { name: 'Review PR URL', exact: true }).click();
  await page.getByRole('dialog').getByRole('heading', { name: 'Review a public PR' }).waitFor();
  await page.getByLabel('GitHub PR URL', { exact: true }).fill('https://evil.example/o/r/pull/1');
  await page.getByRole('button', { name: 'Queue private review', exact: true }).click();
  await page.getByText('Use https://github.com/OWNER/REPO/pull/NUMBER', { exact: true }).waitFor();
  await page.getByLabel('GitHub PR URL', { exact: true }).fill('https://github.com/community/project/pull/7');
  await choose(page, 'External review mode', 'Quick');
  await page.getByRole('button', { name: 'Queue private review', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  await page.getByText('External · private', { exact: true }).waitFor();
  const job = await page.evaluate(async () => {
    const { api } = await window.importDashboard('/src/lib/api.ts');
    return (await api.jobs()).jobs[0];
  });
  assert.equal(job.source, 'external');
  assert.equal(job.publication, 'private');
  assert.equal(job.mode, 'quick');
  assert.equal(job.check_run_url, undefined);
  await page.locator('summary').filter({ hasText: 'Track my external PRs' }).click();
  await page.getByRole('button', { name: 'Find my open PRs', exact: true }).click();
  const actions = page.locator('[aria-label="Choose action for community/small-project #1"]');
  assert.equal(await actions.getByRole('button', { name: 'Track', exact: true }).isEnabled(), false);
  await page.getByRole('link', { name: 'External PR settings', exact: true }).click();
  await page.getByRole('checkbox', { name: 'Track my external PRs', exact: true }).check();
  await page.getByLabel('Check tracked PRs every (minutes)', { exact: true }).fill('30');
  await page.getByRole('button', { name: 'Save external settings', exact: true }).click();
  await page.getByText('External settings saved.', { exact: true }).waitFor();
  await page.getByRole('link', { name: 'Reviews', exact: true }).click();
  await page.locator('summary').filter({ hasText: 'Track my external PRs' }).click();
  await page.getByRole('button', { name: 'Find my open PRs', exact: true }).click();
  await actions.getByRole('button', { name: 'Track', exact: true }).click();
  await page.getByRole('button', { name: 'Untrack', exact: true }).waitFor();
  const ignored = page.locator('[aria-label="Choose action for community/small-project #2"]');
  await ignored.getByRole('button', { name: 'Ignore', exact: true }).click();
  await page.getByText('PR ignored in discovery.', { exact: true }).waitFor();
  await page.locator('[aria-label="Choose action for community/small-project #3"]').getByRole('button', { name: 'Review once', exact: true }).click();
  await page.getByText('Private review queued.', { exact: true }).waitFor();
  await page.getByRole('button', { name: 'Untrack', exact: true }).click();
  await page.getByText('stopped · untracked manually', { exact: true }).waitFor();
  await page.getByRole('button', { name: 'Find my open PRs', exact: true }).click();
  assert.equal(await ignored.count(), 0);
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
  await context.close();
}

try {
  if (!emptyYaml && !process.argv.includes('--screenshots-only')) await checkExternalReviews();
  if (!emptyYaml && !process.argv.includes('--screenshots-only')) await checkDashboardQol();
  if (!emptyYaml && !process.argv.includes('--screenshots-only')) await checkAuditSettings();
  if (!process.argv.includes('--screenshots-only')) await checkThemeAndRetrievalLink();
  if (!process.argv.includes('--screenshots-only')) await checkWizardReranker();
  if (!process.argv.includes('--screenshots-only')) await checkFallbacksAndWebSearch();
  if (!emptyYaml && !process.argv.includes('--checks-only')) {
    for (const scheme of ['light', 'dark']) {
      await shot(
        '/setup?stage=create_app&authorized=0',
        `setup-no-token-${scheme}.png`,
        scheme,
        'Open your setup link',
      );
      await shot(
        '/setup?stage=create_app&token=demo',
        `setup-create-${scheme}.png`,
        scheme,
        'Create your GitHub App',
      );
      await shot(
        '/setup?stage=configure',
        `setup-models-${scheme}.png`,
        scheme,
        'Connect your review models',
      );
      await shot('/', `overview-${scheme}.png`, scheme, 'Overview');
      await shot('/reviews', `reviews-${scheme}.png`, scheme, 'Reviews');
      await shot(
        '/reviews/review-105',
        `review-detail-${scheme}.png`,
        scheme,
        'Handle empty results in repository search',
        async (page) => {
          await page.getByRole('button', { name: 'View log', exact: true }).click();
          await page.getByLabel('Run log output').waitFor();
        },
      );
      await shot(
        '/repositories',
        `repositories-${scheme}.png`,
        scheme,
        'Repositories',
        async (page) => {
          await page.getByLabel('Select northstar/api', { exact: true }).check();
          await page.getByLabel('Select northstar/web', { exact: true }).check();
          await page.getByText('2 selected', { exact: true }).waitFor();
        },
      );
      await shot('/settings', `settings-models-${scheme}.png`, scheme, 'Review models');
      await shot(
        '/settings?tab=review',
        `settings-review-${scheme}.png`,
        scheme,
        'Review behavior',
      );
      await shot(
        '/settings?tab=budgets',
        `settings-budgets-${scheme}.png`,
        scheme,
        'Review limits',
      );
      await shot(
        '/settings?tab=retrieval',
        `settings-retrieval-${scheme}.png`,
        scheme,
        'Repository retrieval',
      );
      await shot(
        '/settings?tab=advanced',
        `settings-advanced-${scheme}.png`,
        scheme,
        'Configuration file',
      );
    }
    {
      const { context, page } = await open('/', 'dark', { width: 390, height: 844 });
      await page.getByRole('heading', { name: 'Overview', exact: true }).waitFor();
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
        true,
        'Mobile page must not overflow horizontally',
      );
      await page.screenshot({ path: join(output, 'overview-mobile-dark.png') });
      await context.close();
    }
    for (const scheme of ['light', 'dark']) {
      for (const [route, name] of [
        ['/', 'Overview'],
        ['/repositories', 'Repositories'],
        ['/reviews', 'Reviews'],
        ['/reviews/review-105', 'Handle empty results in repository search'],
      ]) {
        const { context, page } = await open(route, scheme, { width: 800, height: 1000 });
        await page.getByRole('heading', { name, exact: true }).waitFor();
        if (route === '/repositories') {
          await page.getByLabel('Select northstar/api', { exact: true }).check();
          await page.getByLabel('Select northstar/web', { exact: true }).check();
          const bar = await page.getByLabel('Bulk repository actions').boundingBox();
          assert.ok(
            bar && bar.y >= 0 && bar.y + bar.height <= 1000,
            'Bulk bar stays in the viewport',
          );
        }
        assert.equal(
          await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
          true,
          `${route} fits at 800px`,
        );
        await page.screenshot({
          path: join(
            output,
            `${route === '/' ? 'overview' : route.includes('review-') ? 'review-detail' : route.slice(1)}-800-${scheme}.png`,
          ),
        });
        await context.close();
      }
    }
  }

  if (emptyYaml) {
    // Run against VITE_MOCK=1 VITE_MOCK_EMPTY_YAML=1: no prefilled YAML or keys.
    for (const differentValidator of [false, true]) {
      const { context, page } = await open('/setup?stage=create_app&token=demo');
      await page.getByRole('button', { name: 'Create GitHub App' }).click();
      await page.getByRole('button', { name: 'Sign in with GitHub' }).click();
      await page.getByRole('button', { name: 'Install on repositories' }).click();
      assert.equal(await page.locator('#model-investigator-endpoint').inputValue(), '');
      assert.equal(await page.locator('#model-investigator-name').inputValue(), '');
      await edit(page, 'model-investigator-endpoint', investigator.base_url);
      await edit(page, 'model-investigator-name', investigator.model);
      await firstKey(page);
      await page.getByLabel('Use a different validator model', { exact: true }).click();
      await page.getByText('Validator connection', { exact: true }).click();
      await edit(page, 'model-validator-name', 'different-model');
      if (!differentValidator) await edit(page, 'model-validator-name', investigator.model);
      await page.getByRole('button', { name: 'Test connections', exact: true }).click();
      await page.getByRole('button', { name: 'Finish setup' }).click();
      await page.getByRole('heading', { name: 'Overview', exact: true }).waitFor();
      await page.getByRole('link', { name: 'Settings', exact: true }).click();
      await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
      checkMinimal(
        await page.getByLabel('Review configuration YAML').inputValue(),
        differentValidator,
      );
      assert.equal(
        (await page.locator('body').innerText()).includes('fixture-onboarding-key'),
        false,
      );
      await context.close();
    }
    {
      const { context, page } = await open('/settings');
      await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
      assert.equal(await page.getByLabel('Review configuration YAML').inputValue(), '');
      await page.getByRole('tab', { name: 'Models', exact: true }).click();
      await edit(page, 'model-investigator-endpoint', investigator.base_url);
      await edit(page, 'model-investigator-name', investigator.model);
      await firstKey(page);
      await page.getByRole('button', { name: 'Save changes', exact: true }).click();
      await page.getByText('Settings saved.', { exact: true }).waitFor();
      await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
      checkMinimal(await page.getByLabel('Review configuration YAML').inputValue());
      // Optional nested maps can also be created after starting from nothing.
      await page.getByRole('tab', { name: 'Budgets', exact: true }).click();
      await edit(page, 'budget-run_max_seconds', '600');
      await page.getByRole('tab', { name: 'Retrieval', exact: true }).click();
      await page.getByLabel('Enable Vera retrieval', { exact: true }).click();
      await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
      const doc = parseDocument(await page.getByLabel('Review configuration YAML').inputValue());
      assert.equal(doc.getIn(['budget', 'run_max_seconds']), 600);
      assert.equal(doc.getIn(['vera', 'enabled']), true);
      await context.close();
    }
  }

  if (!emptyYaml && !process.argv.includes('--screenshots-only')) {
    await checkReviewSettings();
    await checkMentionsAndReviewDetail();
    await checkSetupLink();

    // Selects retain labels, keyboard navigation, and empty filter values.
    for (const [scheme, viewport] of [
      ['light', { width: 1440, height: 900 }],
      ['dark', { width: 1440, height: 900 }],
      ['dark', { width: 390, height: 844 }],
    ]) {
      const { context, page } = await open('/reviews', scheme, viewport);
      const status = page.getByRole('button', { name: 'Status', exact: true });
      await status.focus();
      await status.press('Space');
      const menu = page.getByRole('listbox');
      await menu.waitFor();
      assert.ok(await menu.getAttribute('aria-labelledby'), 'Listbox has a label');
      await page.keyboard.press('ArrowDown');
      await page.keyboard.press('Enter');
      await page.waitForFunction(
        () => document.querySelector('.result-count')?.textContent === '1 job',
      );
      assert.equal(await status.innerText(), 'Queued');
      assert.equal(
        await status.evaluate((element) => element === document.activeElement),
        true,
        'Selection returns focus to trigger',
      );
      await status.press('Space');
      await menu.waitFor();
      await page.keyboard.press('End');
      await page.keyboard.press('Escape');
      assert.equal(await status.innerText(), 'Queued', 'Escape does not change selection');
      assert.equal(
        await status.evaluate((element) => element === document.activeElement),
        true,
        'Escape returns focus to trigger',
      );
      await status.press('Space');
      await menu.waitFor();
      await page.keyboard.press('Home');
      await page.keyboard.press('Enter');
      await page.waitForFunction(
        () => document.querySelector('.result-count')?.textContent === '7 jobs',
      );
      assert.equal(
        await status.innerText(),
        'All statuses',
        'Empty value retains its display label',
      );
      const kind = page.getByRole('button', { name: 'Job kind', exact: true });
      await kind.click();
      await page.getByRole('option', { name: 'Indexes only', exact: true }).click();
      await page.getByRole('button', { name: 'Clear filters', exact: true }).click();
      await page.waitForFunction(
        () => document.querySelector('.result-count')?.textContent === '7 jobs',
      );
      assert.equal(await kind.innerText(), 'Reviews and indexes', 'Clear filters resets Select');
      const repo = page.getByRole('button', { name: 'Repository', exact: true });
      await repo.click();
      await page.getByRole('option', { name: 'northstar/api', exact: true }).click();
      await page.waitForFunction(
        () => document.querySelector('.result-count')?.textContent === '3 jobs',
      );
      await repo.click();
      await page.getByRole('option', { name: 'All repositories', exact: true }).click();
      await page.waitForFunction(
        () => document.querySelector('.result-count')?.textContent === '7 jobs',
      );
      assert.equal(await page.locator('select').count(), 0, 'All filters use Bits UI');
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
        true,
        'Selects must not overflow the page',
      );
      await context.close();
    }

    // Follow the full wizard, including the server-authoritative completion check.
    {
      const { context, page } = await open('/setup?stage=create_app&token=demo');
      await page.getByRole('button', { name: 'Create GitHub App' }).click();
      await page.getByRole('button', { name: 'Sign in with GitHub' }).click();
      await page.getByRole('button', { name: 'Install on repositories' }).click();
      await page.getByRole('button', { name: 'Test connections', exact: true }).click();
      await page.getByRole('button', { name: 'Finish setup' }).click();
      await page.getByRole('heading', { name: 'Overview', exact: true }).waitFor();
      await context.close();
    }
    // Newly enabled retrieval maps remain editable, including optional values.
    {
      const { context, page } = await open('/settings');
      await page.getByRole('tab', { name: 'Retrieval', exact: true }).click();
      await page.getByLabel('Enable reranker', { exact: true }).click();
      await page.getByLabel('Reranker format', { exact: true }).click();
      await page.getByRole('option', { name: 'Top-k format', exact: true }).click();
      await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
      const editor = page.getByLabel('Review configuration YAML');
      assert.equal(
        parseDocument(await editor.inputValue()).getIn(['vera', 'reranker', 'protocol']),
        'voyage',
      );
      await page.getByRole('tab', { name: 'Retrieval', exact: true }).click();
      await page.getByLabel('Reranker format', { exact: true }).click();
      await page.getByRole('option', { name: 'Detect from endpoint', exact: true }).click();
      await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
      assert.equal(
        parseDocument(await editor.inputValue()).hasIn(['vera', 'reranker', 'protocol']),
        false,
      );
      await context.close();
    }
    // Form edits, long-form reasoning, and comments share one YAML Document.
    {
      const { context, page } = await open('/settings');
      const input = page.getByLabel('Model', { exact: true }).first();
      await input.fill('edited-model');
      await input.press('Tab');
      await page.getByText('You have unsaved changes', { exact: true }).waitFor();
      await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
      const editor = page.getByLabel('Review configuration YAML');
      let yaml = await editor.inputValue();
      assert.ok(yaml.includes('# Reads the diff and proposes findings.'));
      assert.ok(yaml.includes('model: edited-model'));
      yaml = yaml.replace(
        'reasoning: medium',
        'reasoning:\n      effort: high\n      budget_tokens: 16384 # keep this budget',
      );
      await editor.fill(yaml);
      await page.getByRole('tab', { name: 'Models', exact: true }).click();
      const effort = page.getByLabel('Reasoning effort').first();
      await effort.focus();
      await effort.press('Space');
      await page.getByRole('listbox').waitFor();
      await page.keyboard.type('low');
      await page.keyboard.press('Enter');
      await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
      const changed = await editor.inputValue();
      assert.ok(changed.includes('effort: low'));
      assert.ok(changed.includes('budget_tokens: 16384 # keep this budget'));
      await editor.fill('models: [');
      assert.equal(await page.getByRole('button', { name: 'Save changes' }).isEnabled(), false);
      await page.getByRole('button', { name: 'Discard', exact: true }).click();
      await page.getByRole('tab', { name: 'Models', exact: true }).click();
      await page.getByRole('button', { name: 'Replace key', exact: true }).first().click();
      await page.getByLabel('API key', { exact: true }).fill('fixture-key-not-a-secret');
      await page.getByRole('dialog').getByRole('button', { name: 'Set key', exact: true }).click();
      await page.getByText('API key saved.', { exact: true }).waitFor();
      assert.equal(
        (await page.locator('body').innerText()).includes('fixture-key-not-a-secret'),
        false,
      );
      await page.getByRole('tab', { name: 'Review', exact: true }).click();
      await page.getByLabel('Review draft pull requests').click();
      await page.getByRole('button', { name: 'Save changes' }).click();
      await page.getByText('Settings saved.', { exact: true }).waitFor();
      await context.close();
    }
    {
      const { context, page } = await open('/repositories');
      await choose(page, 'Trigger for northstar/api', 'Auto');
      await page.getByText('northstar/api: trigger saved.', { exact: true }).waitFor();
      await page.getByRole('button', { name: 'Actions for northstar/api', exact: true }).click();
      await page.getByRole('menuitem', { name: 'Review a pull request' }).click();
      await choose(page, 'Review mode', 'Deep');
      await page.getByRole('button', { name: /251 Handle connection timeouts/ }).click();
      await page.getByText('Review queued for northstar/api #251.', { exact: true }).waitFor();
      await page.getByRole('link', { name: 'Reviews', exact: true }).click();
      await page.getByRole('link', { name: 'Manual review', exact: true }).waitFor();
      const saved = await page.evaluate(async () => {
        const { api } = await window.importDashboard('/src/lib/api.ts');
        return api.jobs();
      });
      assert.equal(saved.jobs[0].mode, 'deep');
      assert.equal(saved.jobs[0].trigger, 'manual');
      await context.close();
    }
  }
  assert.deepEqual(errors, [], 'No browser runtime errors');
  if (emptyYaml) {
    console.log(
      'PASS: empty-YAML wizard and Settings, first key, minimal saved routes, ' +
        'validator inheritance, fallback editor/tests, web search, new maps, ' +
        'no browser runtime errors.',
    );
  } else if (process.argv.includes('--screenshots-only')) {
    console.log(
      'PASS: desktop screenshots in both themes, mobile overflow, no browser runtime errors.',
    );
  } else {
    console.log(
      'PASS: setup link authorization/copy, labelled keyboard selects in both themes/mobile, ' +
        'empty filters, wizard, YAML/comments/reasoning, secret dialog, server settings, ' +
        'trigger/mode defaults and overrides, mention notice/commands, fallback editor/tests, ' +
        'web search, manual/rerun modes, live updates, audit settings, repository sorting/filtering/bulk, ' +
        'review search/URL, safe markdown, overview period/sidebar persistence, budget previews, no browser runtime errors.',
    );
  }
} finally {
  await browser.close();
}
