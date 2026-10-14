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
  executablePath: process.env.CHROMIUM_PATH || join(homedir(), '.cache/ms-playwright/chromium-1247/chrome-linux64/chrome'),
  headless: true
});
const errors = [];
async function open(path, scheme = 'light', viewport = { width: 1440, height: 900 }) {
  const context = await browser.newContext({ viewport, colorScheme: scheme, reducedMotion: 'reduce' });
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
  protocol: 'openai-chat', base_url: 'https://api.example.com/v1',
  api_key_env: 'REVERA_MODEL_API_KEY', model: 'first-model'
};
function checkMinimal(yaml, differentValidator = false) {
  const doc = parseDocument(yaml);
  assert.deepEqual(doc.errors, [], 'Saved YAML parses');
  assert.deepEqual(doc.toJS(), { models: {
    investigator,
    ...(differentValidator ? { validator: { ...investigator, model: 'different-model' } } : {})
  } }, 'Only the configured model routes and required fields are saved');
}
async function edit(page, id, value) {
  const input = page.locator(`#${id}`);
  await input.fill(value); await input.press('Tab');
}
async function firstKey(page) {
  await page.getByRole('button', { name: 'Set key', exact: true }).first().click();
  await page.getByLabel('API key', { exact: true }).fill('fixture-onboarding-key');
  await page.getByRole('dialog').getByRole('button', { name: 'Set key', exact: true }).click();
  await page.getByRole('dialog').waitFor({ state: 'hidden' });
  try { await page.getByText('Set in dashboard', { exact: true }).first().waitFor(); }
  catch (error) { console.error('First-key status:', await page.locator('body').innerText()); throw error; }
}

try {
  if (!emptyYaml && !process.argv.includes('--checks-only')) {
    for (const scheme of ['light', 'dark']) {
      await shot('/setup?stage=create_app&token=demo', `setup-create-${scheme}.png`, scheme, 'Create your GitHub App');
      await shot('/setup?stage=configure', `setup-models-${scheme}.png`, scheme, 'Connect your review models');
      await shot('/', `overview-${scheme}.png`, scheme, 'Overview');
      await shot('/reviews', `reviews-${scheme}.png`, scheme, 'Reviews');
      await shot('/reviews/review-105', `review-detail-${scheme}.png`, scheme, 'Handle empty results in repository search', async (page) => {
        await page.getByRole('button', { name: 'View log', exact: true }).click();
        await page.getByLabel('Run log output').waitFor();
      });
      await shot('/repositories', `repositories-${scheme}.png`, scheme, 'Repositories');
      await shot('/settings', `settings-models-${scheme}.png`, scheme, 'Review models');
      await shot('/settings?tab=advanced', `settings-advanced-${scheme}.png`, scheme, 'Configuration file');
    }
  {
    const { context, page } = await open('/', 'dark', { width: 390, height: 844 });
    await page.getByRole('heading', { name: 'Overview', exact: true }).waitFor();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'Mobile page must not overflow horizontally');
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
       checkMinimal(await page.getByLabel('Review configuration YAML').inputValue(), differentValidator);
       assert.equal((await page.locator('body').innerText()).includes('fixture-onboarding-key'), false);
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

  // Selects retain labels, keyboard navigation, and empty filter values.
  for (const [scheme, viewport] of [
    ['light', { width: 1440, height: 900 }],
    ['dark', { width: 1440, height: 900 }],
    ['dark', { width: 390, height: 844 }]
  ]) {
    const { context, page } = await open('/reviews', scheme, viewport);
    const status = page.getByRole('button', { name: 'Status', exact: true });
    await status.focus(); await status.press('Space');
    const menu = page.getByRole('listbox');
    await menu.waitFor();
    assert.ok(await menu.getAttribute('aria-labelledby'), 'Listbox has a label');
    await page.keyboard.press('ArrowDown'); await page.keyboard.press('Enter');
    await page.waitForFunction(() => document.querySelector('.result-count')?.textContent === '1 job');
    assert.equal(await status.innerText(), 'Queued');
    assert.equal(await status.evaluate((element) => element === document.activeElement), true, 'Selection returns focus to trigger');
    await status.press('Space'); await menu.waitFor();
    await page.keyboard.press('End'); await page.keyboard.press('Escape');
    assert.equal(await status.innerText(), 'Queued', 'Escape does not change selection');
    assert.equal(await status.evaluate((element) => element === document.activeElement), true, 'Escape returns focus to trigger');
    await status.press('Space'); await menu.waitFor();
    await page.keyboard.press('Home'); await page.keyboard.press('Enter');
    await page.waitForFunction(() => document.querySelector('.result-count')?.textContent === '7 jobs');
    assert.equal(await status.innerText(), 'All statuses', 'Empty value retains its display label');
    const kind = page.getByRole('button', { name: 'Job kind', exact: true });
    await kind.click(); await page.getByRole('option', { name: 'Indexes only', exact: true }).click();
    await page.getByRole('button', { name: 'Clear filters', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('.result-count')?.textContent === '7 jobs');
    assert.equal(await kind.innerText(), 'Reviews and indexes', 'Clear filters resets Select');
    const repo = page.getByRole('button', { name: 'Repository', exact: true });
    await repo.click(); await page.getByRole('option', { name: 'northstar/api', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('.result-count')?.textContent === '3 jobs');
    await repo.click(); await page.getByRole('option', { name: 'All repositories', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('.result-count')?.textContent === '7 jobs');
    assert.equal(await page.locator('select').count(), 0, 'All filters use Bits UI');
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'Selects must not overflow the page');
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
    assert.equal(parseDocument(await editor.inputValue()).getIn(['vera', 'reranker', 'protocol']), 'voyage');
    await page.getByRole('tab', { name: 'Retrieval', exact: true }).click();
    await page.getByLabel('Reranker format', { exact: true }).click();
    await page.getByRole('option', { name: 'Detect from endpoint', exact: true }).click();
    await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
    assert.equal(parseDocument(await editor.inputValue()).hasIn(['vera', 'reranker', 'protocol']), false);
    await context.close();
  }
  // Form edits, long-form reasoning, and comments share one YAML Document.
  {
    const { context, page } = await open('/settings');
    const input = page.getByLabel('Model', { exact: true }).first();
    await input.fill('edited-model'); await input.press('Tab');
    await page.getByText('You have unsaved changes', { exact: true }).waitFor();
    await page.getByRole('tab', { name: 'Advanced', exact: true }).click();
    const editor = page.getByLabel('Review configuration YAML');
    let yaml = await editor.inputValue();
    assert.ok(yaml.includes('# Reads the diff and proposes findings.'));
    assert.ok(yaml.includes('model: edited-model'));
    yaml = yaml.replace('reasoning: medium', 'reasoning:\n      effort: high\n      budget_tokens: 16384 # keep this budget');
    await editor.fill(yaml);
    await page.getByRole('tab', { name: 'Models', exact: true }).click();
    const effort = page.getByLabel('Reasoning effort').first();
    await effort.focus(); await effort.press('Space');
    await page.getByRole('listbox').waitFor();
    await page.keyboard.type('low'); await page.keyboard.press('Enter');
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
    assert.equal((await page.locator('body').innerText()).includes('fixture-key-not-a-secret'), false);
    await page.getByRole('tab', { name: 'Review', exact: true }).click();
    await page.getByLabel('Review draft pull requests').click();
    await page.getByRole('button', { name: 'Save changes' }).click();
    await page.getByText('Settings saved.', { exact: true }).waitFor();
    await context.close();
  }
  {
    const { context, page } = await open('/repositories');
    const toggle = page.getByLabel('Enable reviews for northstar/api');
    await toggle.click();
    await page.getByText('northstar/api: reviews paused.', { exact: true }).waitFor();
    assert.equal(await toggle.getAttribute('aria-checked'), 'false');
    await page.getByRole('button', { name: 'Actions for northstar/api', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Review a pull request' }).click();
    await page.getByRole('button', { name: /251 Handle connection timeouts/ }).click();
    await page.getByText('Review queued for northstar/api #251.', { exact: true }).waitFor();
    await page.getByRole('link', { name: 'Reviews', exact: true }).click();
    await page.getByRole('link', { name: 'Manual review', exact: true }).waitFor();
    await context.close();
  }
  }
  assert.deepEqual(errors, [], 'No browser runtime errors');
   console.log(emptyYaml ? 'PASS: empty-YAML wizard and Settings, first key, minimal saved routes, validator inheritance, new maps, no browser runtime errors.' : process.argv.includes('--screenshots-only')
    ? 'PASS: desktop screenshots in both themes, mobile overflow, no browser runtime errors.'
    : 'PASS: labelled keyboard selects in both themes/mobile, empty filters, wizard, YAML/comments/reasoning, secret dialog, server settings, repository toggle, manual review, live updates, no browser runtime errors.');
} finally { await browser.close(); }
