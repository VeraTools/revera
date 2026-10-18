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
  page.on('pageerror', (error) => errors.push(error.message));
  await page.goto(`${base}${path}`);
  await page.waitForFunction(() => !document.body.innerText.includes('Loading '));
  return { context, page };
}
async function shot(path, filename, scheme, ready, action) {
  const { context, page } = await open(path, scheme);
  await page.getByRole('heading', { name: ready, exact: true }).waitFor();
  await action?.(page);
  await page.screenshot({ path: join(output, filename) });
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
    const { setMockStage } = await import(path);
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
    const { api } = await import(path);
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
    const { api } = await import(path);
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
      const { api } = await import(path);
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
    const { api } = await import('/src/lib/api.ts');
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
  await choose(page, 'Filter by trigger', 'Off');
  assert.equal(await page.locator('tbody tr').count(), 1);
  await choose(page, 'Filter by trigger', 'All triggers');
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
    const { api } = await import('/src/lib/api.ts');
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
      const { api } = await import('/src/lib/api.ts');
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
      'Investigator switched from gpt-5.2 to claude-sonnet-4-5: rate limit or server error.',
      { exact: true },
    )
    .waitFor();
  await page.getByText('Validator skipped gemini-3-pro: key not set.', { exact: true }).waitFor();
  assert.equal(await page.getByLabel('Review mode', { exact: true }).innerText(), 'Default (Deep)');
  await choose(page, 'Review mode', 'Quick');
  await page.getByRole('button', { name: 'Re-run review', exact: true }).click();
  await page.getByText('A new review has been queued.', { exact: true }).waitFor();
  const jobs = await page.evaluate(async () => {
    const { api } = await import('/src/lib/api.ts');
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

try {
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
      await shot('/repositories', `repositories-${scheme}.png`, scheme, 'Repositories');
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
        const { api } = await import('/src/lib/api.ts');
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
        'web search, manual/rerun modes, live updates, audit settings, no browser runtime errors.',
    );
  }
} finally {
  await browser.close();
}
