import assert from 'node:assert/strict';
import { join } from 'node:path';

// Fix coverage is isolated so other dashboard lanes can extend their own checks.
export async function checkFixes({ open, choose, edit, savedConfig }) {
  {
    const { context, page } = await open('/fixes');
    await page.locator('tbody tr').last().waitFor();
    assert.equal(await page.locator('tbody tr').count(), 10);
    const navigation = await page.locator('nav a').allTextContents();
    assert.deepEqual(
      navigation.map((value) => value.trim()),
      ['Overview', 'Pull requests', 'Reviews', 'Fixes', 'Repositories', 'Settings'],
    );
    await choose(page, 'Fix state', 'ready');
    assert.equal(await page.locator('tbody tr').count(), 4);
    assert.equal(new URL(page.url()).searchParams.get('state'), 'ready');
    await choose(page, 'Fix state', 'unsupported');
    await page.getByText('No fix requests in this state.', { exact: true }).waitFor();
    await context.close();
  }
  {
    const { context, page } = await open('/fixes/new?job=review-104');
    await page
      .getByRole('button', { name: 'Generate preview (this run only)', exact: true })
      .waitFor();
    assert.equal(await page.getByRole('checkbox').count(), 1);
    assert.equal(await page.getByRole('checkbox').isChecked(), true);
    await page.getByRole('checkbox').uncheck();
    assert.equal(await page.locator('.notice.error').count(), 0);
    assert.equal(
      await page.getByRole('button', { name: 'Generate preview (this run only)' }).isEnabled(),
      false,
    );
    await page.getByRole('checkbox').check();
    await page
      .getByRole('button', { name: 'Generate preview (this run only)', exact: true })
      .click();
    await page.getByText('This run only', { exact: true }).waitFor();
    const id = new URL(page.url()).pathname.split('/').at(-1);
    const result = await page.evaluate(async (id) => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const detail = await api.fix(id);
      const duplicate = await api.requestFix(
        'review-104',
        detail.fix.findings.map((f) => f.id),
        true,
      );
      return { detail, duplicate: duplicate.fix.id, settings: await api.fixSettings() };
    }, id);
    assert.equal(result.detail.fix.state, 'queued');
    assert.equal(result.detail.fix.consent, 'once');
    assert.equal(
      result.duplicate,
      id,
      'Same accepted findings and SHA converge on the active request',
    );
    assert.equal(
      result.settings.fixes.execution,
      false,
      'Once consent never enables persistent execution',
    );
    await page.evaluate(async (id) => {
      const { setMockFixState } = await window.importDashboard('/src/lib/mock.ts');
      setMockFixState(id, 'generating', 'Inspecting the selected finding in the sandbox.');
    }, id);
    await page
      .getByText('Inspecting the selected finding in the sandbox.', { exact: true })
      .waitFor();
    await page.getByRole('button', { name: 'Cancel request', exact: true }).click();
    await page.locator('.detail-header').getByText('Cancelled', { exact: true }).waitFor();
    assert.equal(
      await page.getByRole('button', { name: 'Cancel request', exact: true }).count(),
      0,
    );
    await page.getByRole('link', { name: 'review-104', exact: true }).click();
    await page.getByRole('link', { name: 'Fix with Revera', exact: true }).waitFor();
    await page
      .getByRole('region', { name: 'Review fix requests' })
      .getByRole('link', { name: id, exact: true })
      .waitFor();
    await context.close();
  }
  {
    const { context, page } = await open('/reviews/review-105');
    const offer = page.getByRole('link', { name: 'Fix with Revera', exact: true });
    await offer.waitFor();
    assert.equal(await offer.getAttribute('href'), '/fixes/new?job=review-105');
    await page
      .getByRole('region', { name: 'Review fix requests' })
      .getByRole('link', { name: 'fix-confirm', exact: true })
      .waitFor();
    await offer.click();
    await page.getByRole('link', { name: 'Open existing request', exact: true }).waitFor();
    assert.equal(await page.getByRole('button', { name: /^Generate preview/ }).count(), 0);
    await page.getByRole('link', { name: 'Open existing request', exact: true }).click();
    const confirm = page.getByRole('button', { name: 'Confirm preview', exact: true });
    await confirm.waitFor();
    assert.equal(await confirm.isEnabled(), false);
    await page
      .getByRole('checkbox', { name: 'Allow generation for this run only', exact: true })
      .check();
    await confirm.click();
    await page.getByText('This run only', { exact: true }).waitFor();
    await page.locator('.detail-header').getByText('Queued', { exact: true }).waitFor();
    await context.close();
  }
  for (const execution of [true, false]) {
    const { context, page } = await open('/fixes');
    await page.locator('tbody tr').last().waitFor();
    await page.evaluate(async (execution) => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const settings = await api.fixSettings();
      await api.saveFixSettings({ ...settings.fixes, execution: !execution });
      await api.repoFixSettings('northstar/worker', { execution });
    }, execution);
    await page.locator('a[href="/fixes/fix-confirm"]').click();
    const confirm = page.getByRole('button', { name: 'Confirm preview', exact: true });
    await confirm.waitFor();
    if (execution) {
      await page.waitForFunction(() =>
        [...document.querySelectorAll('button')].some(
          (button) => button.textContent === 'Confirm preview' && !button.disabled,
        ),
      );
      assert.equal(
        await page
          .getByRole('checkbox', { name: 'Allow generation for this run only', exact: true })
          .count(),
        0,
      );
    } else {
      await page
        .getByRole('checkbox', { name: 'Allow generation for this run only', exact: true })
        .waitFor();
      assert.equal(
        await confirm.isEnabled(),
        false,
        'A disabled repo override still requires consent when the global policy is enabled',
      );
      await page
        .getByRole('checkbox', { name: 'Allow generation for this run only', exact: true })
        .check();
    }
    await confirm.click();
    await page.locator('.detail-header').getByText('Queued', { exact: true }).waitFor();
    await page
      .getByText(execution ? 'Persistent execution' : 'This run only', { exact: true })
      .waitFor();
    await context.close();
  }
  for (const id of ['review-103', 'review-101']) {
    const { context, page } = await open(`/reviews/${id}`);
    await page.getByRole('heading', { name: 'Summary', exact: true }).waitFor();
    assert.equal(await page.getByRole('link', { name: 'Fix with Revera', exact: true }).count(), 0);
    await page.getByRole('link', { name: 'Fixes', exact: true }).click();
    await page.evaluate(async (id) => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      window.fixBlockedPlan = await api.fixPlan(id);
    }, id);
    const plan = await page.evaluate(() => window.fixBlockedPlan);
    assert.ok(plan.blocked);
    await context.close();
  }
  {
    const { context, page } = await open('/fixes/fix-ready');
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await page.getByRole('region', { name: 'Patch diff', exact: true }).waitFor();
    assert.equal(await page.locator('details.diff-file').count(), 2);
    assert.ok(await page.locator('.diff-file .add').count());
    assert.ok(await page.locator('.diff-file .remove').count());
    await page.locator('.diff-file summary').first().click();
    assert.equal(await page.locator('.diff-file').first().getAttribute('open'), null);
    await page.locator('.diff-file summary').first().click();
    await page.getByText('Test output', { exact: true }).click();
    await page.getByText('2 tests passed. Network disabled.', { exact: false }).waitFor();
    await page.getByRole('button', { name: 'Copy agent instructions', exact: true }).click();
    await page.getByText('Agent instructions copied.', { exact: true }).waitFor();
    assert.ok((await page.evaluate(() => navigator.clipboard.readText())).includes('abc123456789'));
    const downloadPromise = page.waitForEvent('download');
    await page.getByRole('button', { name: 'Download patch', exact: true }).click();
    const download = await downloadPromise;
    assert.ok(download.suggestedFilename().endsWith('patch.diff'));
    const stream = await download.createReadStream();
    const chunks = [];
    for await (const chunk of stream) chunks.push(chunk);
    assert.ok(Buffer.concat(chunks).toString().includes('diff --git a/src/search/results.test.ts'));
    const approve = page.getByRole('button', { name: 'Approve & publish', exact: true });
    assert.equal(await approve.isEnabled(), false, 'Patch download is not a write destination');
    assert.equal(
      await page.getByRole('radio', { name: 'Commit to PR branch', exact: true }).isEnabled(),
      false,
    );
    await page.getByRole('radio', { name: 'Draft pull request', exact: true }).check();
    assert.equal(await approve.isEnabled(), true);
    page.once('dialog', (dialog) => dialog.dismiss());
    await approve.click();
    await page.locator('.detail-header').getByText('Ready', { exact: true }).waitFor();
    assert.equal(
      await page.evaluate(async () => {
        const { api } = await window.importDashboard('/src/lib/api.ts');
        return (await api.fix('fix-ready')).fix.state;
      }),
      'ready',
      'Dismissing approval must not publish',
    );
    page.once('dialog', (dialog) => dialog.accept());
    await approve.click();
    await page.locator('.detail-header').getByText('Verifying', { exact: true }).waitFor();
    await page.getByRole('link', { name: 'Open draft pull request', exact: false }).waitFor();
    assert.equal(await approve.count(), 0);
    await context.close();
  }
  for (const [id, reason] of [
    ['fix-external', 'Revera is not installed on this repository'],
    ['fix-readonly', "Needs Contents: write — update the app's permissions"],
    ['fix-fork', 'The PR branch lives in a fork'],
  ]) {
    const { context, page } = await open(`/fixes/${id}`);
    await page.getByText(reason, { exact: true }).first().waitFor();
    assert.equal(
      await page.getByRole('button', { name: 'Approve & publish', exact: true }).isEnabled(),
      false,
    );
    assert.equal(
      await page.getByRole('radio', { name: 'Draft pull request', exact: true }).isEnabled(),
      false,
    );
    if (id === 'fix-readonly')
      await page
        .getByRole('link', { name: 'Update app permissions', exact: true })
        .first()
        .waitFor();
    if (id === 'fix-fork')
      await page.getByText('Revera cannot push to contributor forks', { exact: true }).waitFor();
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
      true,
      'Diff and capability reasons fit mobile',
    );
    await context.close();
  }
  {
    const { context, page } = await open('/fixes/fix-invalid');
    await page.getByText('Regenerate preview', { exact: true }).click();
    await page
      .getByLabel('Optional guidance', { exact: true })
      .fill('Tie the guard to the active request.');
    await page.getByRole('button', { name: 'Regenerate', exact: true }).click();
    await page.locator('.detail-header').getByText('Queued', { exact: true }).waitFor();
    const result = await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      return api.fix('fix-invalid');
    });
    assert.equal(result.fix.attempt, 2);
    assert.equal(result.fix.events.at(-1).detail, 'Tie the guard to the active request.');
    await page.getByRole('button', { name: 'Cancel request', exact: true }).click();
    await page.getByText('Regenerate preview', { exact: true }).click();
    assert.equal(
      await page.getByRole('button', { name: 'Regenerate', exact: true }).isEnabled(),
      false,
    );
    await page
      .getByText('Attempt limit reached. Request a new review before trying again.', {
        exact: true,
      })
      .waitFor();
    page.once('dialog', (dialog) => dialog.accept());
    await page.getByRole('button', { name: 'Discard preview', exact: true }).click();
    await page.locator('.detail-header').getByText('Discarded', { exact: true }).waitFor();
    assert.equal(
      await page.getByRole('button', { name: 'Discard preview', exact: true }).count(),
      0,
    );
    await context.close();
  }
  {
    const { context, page } = await open('/fixes/fix-verified');
    await page.getByRole('heading', { name: 'Follow-up review', exact: true }).waitFor();
    await page
      .getByText('complete · 1 resolved · 1 still open · 0 new findings', { exact: true })
      .waitFor();
    await context.close();
    const stale = await open('/fixes/fix-stale');
    await stale.page
      .getByText('PR changed since the review; request a new review', { exact: true })
      .waitFor();
    assert.equal(
      await stale.page.getByRole('button', { name: 'Approve & publish', exact: true }).count(),
      0,
    );
    await stale.context.close();
  }
  {
    const { context, page } = await open('/settings?tab=fixes');
    await page.getByRole('switch', { name: 'Enable fix execution', exact: true }).waitFor();
    assert.equal(
      await page
        .getByRole('switch', { name: 'Enable fix execution', exact: true })
        .getAttribute('aria-checked'),
      'false',
    );
    const original = await savedConfig(page);
    await page.getByRole('switch', { name: 'Enable fix execution', exact: true }).click();
    await choose(page, 'Fix trigger', 'After accepted review');
    await page.getByText('Delivery and requester permissions', { exact: true }).click();
    await page.getByRole('switch', { name: 'Allow commits to the PR branch', exact: true }).click();
    await choose(page, 'Preferred delivery', 'Commit to PR branch');
    assert.equal(
      await page
        .getByRole('switch', { name: 'Require approval before GitHub writes', exact: true })
        .isEnabled(),
      false,
    );
    await choose(page, 'Who may request fixes', 'Writers, maintainers and admins');
    await page.getByRole('switch', { name: 'Allow external fix previews', exact: true }).click();
    await page.getByText('Sandbox, tests and resource limits', { exact: true }).click();
    await page
      .getByLabel('Test command (argv)', { exact: true })
      .fill('cargo test "search regression"');
    await edit(page, 'fix-limit-model_requests', '30');
    await edit(page, 'fix-limit-max_files', '8');
    await page.getByText('Fix model routes', { exact: true }).click();
    await page.getByRole('switch', { name: 'Override fixer model', exact: true }).click();
    await edit(page, 'model-fixer-name', 'fix-model');
    const card = page.locator('section[aria-label="fixer"]');
    await card.locator('summary').filter({ hasText: 'Fallback models' }).click();
    await card.getByRole('button', { name: 'Add fallback', exact: true }).click();
    const fallback = page.locator('#model-fixer-fallback-1-name');
    await fallback.fill('fix-backup');
    await fallback.press('Tab');
    await page.getByRole('switch', { name: 'Override fix verifier model', exact: true }).click();
    await edit(page, 'model-fix_verifier-name', 'verify-model');
    await page.getByRole('button', { name: 'Save changes', exact: true }).click();
    await page.getByText('Settings saved.', { exact: true }).waitFor();
    const doc = await savedConfig(page);
    assert.deepEqual(
      doc.getIn(['models', 'investigator']).toJSON(),
      original.getIn(['models', 'investigator']).toJSON(),
    );
    assert.equal(doc.getIn(['models', 'fixer', 'model']), 'fix-model');
    assert.equal(doc.getIn(['models', 'fixer', 'fallback', 1, 'model']), 'fix-backup');
    assert.equal(doc.getIn(['models', 'fix_verifier', 'model']), 'verify-model');
    const settings = await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const test = api.test;
      window.fixTests = [];
      api.test = (...args) => {
        window.fixTests.push(args);
        return test(...args);
      };
      return api.fixSettings();
    });
    assert.equal(settings.fixes.execution, true);
    assert.equal(settings.fixes.trigger, 'after_review');
    assert.equal(settings.fixes.delivery, 'same_branch');
    assert.equal(settings.fixes.require_approval, true);
    assert.equal(settings.fixes.requesters, 'writers');
    assert.equal(settings.fixes.external, true);
    assert.equal(settings.fixes.limits.model_requests, 30);
    assert.equal(settings.fixes.limits.max_files, 8);
    assert.deepEqual(settings.fixes.test_command, ['cargo', 'test', 'search regression']);
    await page.getByRole('button', { name: 'Test fix model', exact: true }).click();
    await card.getByText('342 ms', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Test fix verifier', exact: true }).click();
    assert.deepEqual(await page.evaluate(() => window.fixTests), [['fixer'], ['fix_verifier']]);
    await page.getByRole('switch', { name: 'Override fixer model', exact: true }).click();
    await page.getByRole('button', { name: 'Save changes', exact: true }).click();
    await page.getByText('Settings saved.', { exact: true }).waitFor();
    assert.equal(
      (await savedConfig(page)).hasIn(['models', 'fixer']),
      false,
      'Inherit removes only the fix route override',
    );
    await page.getByLabel('Test command (argv)', { exact: true }).fill('cargo "unterminated');
    await page.getByText('Close the quote or escape before saving.', { exact: true }).waitFor();
    assert.equal(
      await page.getByRole('button', { name: 'Save changes', exact: true }).count(),
      0,
      'Invalid unsaved command cannot create a policy save',
    );
    await page.getByLabel('Test command (argv)', { exact: true }).fill('cargo test');
    await page.getByRole('button', { name: 'Save changes', exact: true }).click();
    await page.getByText('Settings saved.', { exact: true }).waitFor();
    await page.getByRole('link', { name: 'Repositories', exact: true }).click();
    await page.getByRole('button', { name: 'Actions for northstar/api', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Fix settings', exact: true }).click();
    await page
      .getByRole('dialog')
      .getByRole('heading', { name: 'Repository Fixes', exact: true })
      .waitFor();
    await choose(page, 'Fix execution', 'Disabled');
    await choose(page, 'Fix trigger', 'Manual only');
    await choose(page, 'Fix delivery', 'Patch preview');
    await page.getByRole('button', { name: 'Save fix override', exact: true }).click();
    await page.getByText('Repository fix settings saved.', { exact: true }).waitFor();
    const override = await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      return (await api.fixSettings()).repos['northstar/api'];
    });
    assert.deepEqual(override, { execution: false, trigger: 'manual', delivery: 'patch' });
    await page.getByRole('button', { name: 'Reset to defaults', exact: true }).click();
    await page.waitForFunction(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      return !('northstar/api' in (await api.fixSettings()).repos);
    });
    await context.close();
  }
}

export async function fixScreenshots({ open, choose, output }) {
  for (const scheme of ['light', 'dark']) {
    for (const [path, name] of [
      ['/fixes', 'fixes-list'],
      ['/fixes/new?job=review-104', 'fixes-plan'],
      ['/fixes/fix-ready', 'fixes-preview'],
      ['/fixes/fix-external', 'fixes-external'],
      ['/fixes/fix-verified', 'fixes-verified'],
      ['/settings?tab=fixes', 'fixes-settings'],
      ['/reviews/review-105', 'fixes-review-detail'],
      ['/repositories', 'fixes-repo-override'],
    ]) {
      const { context, page } = await open(path, scheme);
      if (name === 'fixes-list') await page.locator('tbody tr').last().waitFor();
      if (name === 'fixes-plan')
        await page
          .getByRole('button', { name: 'Generate preview (this run only)', exact: true })
          .waitFor();
      if (name === 'fixes-preview' || name === 'fixes-external')
        await page.getByRole('region', { name: 'Patch diff', exact: true }).waitFor();
      if (name === 'fixes-verified')
        await page.getByRole('heading', { name: 'Follow-up review', exact: true }).waitFor();
      if (name === 'fixes-settings') {
        await page.getByText('Delivery and requester permissions', { exact: true }).click();
        await page.getByText('Fix model routes', { exact: true }).click();
        await page.getByText('Sandbox, tests and resource limits', { exact: true }).click();
      }
      if (name === 'fixes-review-detail') {
        await page.getByRole('link', { name: 'Fix with Revera', exact: true }).waitFor();
        await page
          .getByRole('region', { name: 'Review fix requests' })
          .getByRole('link', { name: 'fix-confirm', exact: true })
          .waitFor();
      }
      if (name === 'fixes-repo-override') {
        await page.getByRole('button', { name: 'Actions for northstar/api', exact: true }).click();
        await page.getByRole('menuitem', { name: 'Fix settings', exact: true }).click();
        await choose(page, 'Fix execution', 'Disabled');
        await choose(page, 'Fix trigger', 'Manual only');
        await choose(page, 'Fix delivery', 'Patch preview');
        await page.getByRole('button', { name: 'Save fix override', exact: true }).click();
        await page.getByText('Repository fix settings saved.', { exact: true }).waitFor();
      }
      await page.evaluate(() => window.scrollTo(0, 0));
      await page.screenshot({
        path: join(output, `${name}-${scheme}.png`),
        fullPage: name !== 'fixes-repo-override',
      });
      await context.close();
    }
  }
}
