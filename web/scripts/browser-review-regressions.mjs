import assert from 'node:assert/strict';

// Flush Svelte effects and completed mock promises without relying on a sleep.
const settle = (page) =>
  page.evaluate(
    () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
  );

export async function checkReviewRegressions({ open, choose, edit }) {
  {
    const { context, page } = await open('/fixes/new?job=review-105');
    await page.getByRole('link', { name: 'Open existing request', exact: true }).waitFor();
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const plan = api.fixPlan;
      window.emptyPlans = 0;
      api.fixPlan = (job, ids) => {
        if (ids?.length === 0) {
          window.emptyPlans++;
          throw new Error('Invalid ID prefix');
        }
        if (window.failSelection) throw new Error('Selection failed');
        return plan(job, ids);
      };
      window.failSelection = true;
    });
    await page.getByRole('checkbox').first().uncheck();
    await page.getByText('Selection failed', { exact: true }).waitFor();
    await page.evaluate(() => {
      window.failSelection = false;
    });
    await page.getByRole('checkbox').first().check();
    await page.getByRole('alert').waitFor({ state: 'hidden' });
    for (const checkbox of await page.getByRole('checkbox').all()) await checkbox.uncheck();
    await settle(page);
    assert.equal(await page.evaluate(() => window.emptyPlans), 0);
    assert.equal(await page.getByRole('alert').count(), 0);
    assert.equal(
      await page.getByRole('link', { name: 'Open existing request', exact: true }).count(),
      0,
    );
    await context.close();
  }
  {
    const { context, page } = await open('/reviews');
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      window.originalFixes = api.fixes;
      api.fixes = async () => {
        throw new Error('Fix list unavailable');
      };
    });
    await page.getByRole('link', { name: 'Fixes', exact: true }).click();
    await page.getByText('Fix list unavailable', { exact: true }).waitFor();
    assert.equal(await page.locator('.empty-state').count(), 0);
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      api.fixes = window.originalFixes;
    });
    await page.getByRole('button', { name: 'Try again', exact: true }).click();
    await page.locator('tbody tr').last().waitFor();
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const { live } = await window.importDashboard('/src/lib/events.ts');
      window.listFixture = (await api.fixes()).fixes[0];
      window.listReplies = [];
      api.fixes = () => new Promise((resolve) => window.listReplies.push(resolve));
      live.update((s) => ({ ...s, fix: { ...window.listFixture, updated_at: 'first' } }));
    });
    await page.waitForFunction(() => window.listReplies.length === 1);
    await page.evaluate(async () => {
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, fix: { ...window.listFixture, updated_at: 'second' } }));
    });
    await page.waitForFunction(() => window.listReplies.length === 2);
    await page.evaluate(() =>
      window.listReplies[1]({ fixes: [{ ...window.listFixture, pr_title: 'Newest response' }] }),
    );
    await page.getByText('Newest response', { exact: true }).waitFor();
    await page.evaluate(() =>
      window.listReplies[0]({ fixes: [{ ...window.listFixture, pr_title: 'Old response' }] }),
    );
    await settle(page);
    assert.equal(await page.getByText('Newest response', { exact: true }).count(), 1);
    assert.equal(await page.getByText('Old response', { exact: true }).count(), 0);
    await context.close();
  }
  {
    const { context, page } = await open('/fixes/fix-ready');
    await page.getByRole('region', { name: 'Patch diff', exact: true }).waitFor();
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const { live } = await window.importDashboard('/src/lib/events.ts');
      const original = api.fix;
      window.fixLoads = 0;
      api.fix = (...args) => {
        window.fixLoads++;
        return original(...args);
      };
      const fix = (await original('fix-ready')).fix;
      live.update((s) => ({ ...s, fix }));
    });
    await page.waitForFunction(() => window.fixLoads === 1);
    await page.evaluate(async () => {
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, repos: s.repos + 1, connected: !s.connected }));
    });
    await settle(page);
    assert.equal(
      await page.evaluate(() => window.fixLoads),
      1,
      'Unrelated events do not reload a fix',
    );
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const { live } = await window.importDashboard('/src/lib/events.ts');
      const original = api.fix;
      api.fix = async (...args) => {
        const result = await original(...args);
        result.patch = 'diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n--- comment\n+++i;\n';
        return result;
      };
      live.update((s) => ({ ...s, fix: { ...s.fix } }));
    });
    await page.locator('.diff-file .add').filter({ hasText: '+++i;' }).waitFor();
    assert.equal(
      await page.locator('.diff-file .remove').filter({ hasText: '--- comment' }).count(),
      1,
    );
    assert.equal(await page.locator('.diff-file .add').filter({ hasText: '+++ b/x' }).count(), 0);
    const delivered = await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const settings = await api.fixSettings();
      await api.saveFixSettings({ ...settings.fixes, allow_same_branch: true });
      return (await api.fixAction('fix-ready', 'approve', { delivery: 'same_branch' })).fix;
    });
    assert.equal(delivered.delivery.branch, 'feature/search');
    assert.equal(delivered.delivery.pr, delivered.pr);
    assert.equal(delivered.delivery.url, delivered.pr_url);
    await page.getByRole('link', { name: 'All fixes', exact: false }).evaluate((link) => {
      link.href = '/fixes/missing';
    });
    await page.getByRole('link', { name: 'All fixes', exact: false }).click();
    await page.getByText('Fix request not found.', { exact: true }).waitFor();
    assert.equal(
      await page.locator('.detail-header').count(),
      0,
      'The old fix is not displayed under another ID',
    );
    await context.close();
  }
  {
    const { context, page } = await open('/fixes');
    await page.locator('tbody tr').last().waitFor();
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const settings = await api.fixSettings();
      await api.saveFixSettings({ ...settings.fixes, execution: true });
    });
    await page.locator('a[href="/fixes/fix-confirm"]').click();
    const confirm = page.getByRole('button', { name: 'Confirm preview', exact: true });
    await page.waitForFunction(() =>
      [...document.querySelectorAll('button')].some(
        (b) => b.textContent === 'Confirm preview' && !b.disabled,
      ),
    );
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const detail = await api.fix('fix-confirm');
      api.fix = async () => ({
        ...detail,
        fix: { ...detail.fix, id: 'other-confirm', repo: 'other/repo' },
      });
      api.fixSettings = async () => {
        throw new Error('Settings unavailable');
      };
    });
    await page.getByRole('link', { name: 'All fixes', exact: false }).evaluate((link) => {
      link.href = '/fixes/other-confirm';
    });
    await page.getByRole('link', { name: 'All fixes', exact: false }).click();
    await page.getByText('Settings unavailable', { exact: true }).waitFor();
    assert.equal(
      await confirm.isEnabled(),
      false,
      'Execution permission is not inherited from the previous repository',
    );
    await context.close();
  }
  {
    const { context, page } = await open('/reviews/review-105');
    await page.getByRole('region', { name: 'Review fix requests' }).waitFor();
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const { live } = await window.importDashboard('/src/lib/events.ts');
      const original = api.fixes;
      window.reviewFixLoads = 0;
      api.fixes = (...args) => {
        window.reviewFixLoads++;
        return original(...args);
      };
      live.update((s) => ({ ...s, fix: { id: 'unrelated', review_job: 'another-review' } }));
    });
    await settle(page);
    assert.equal(await page.evaluate(() => window.reviewFixLoads), 0);
    await page.evaluate(async () => {
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, fix: { id: 'related', review_job: 'review-105' } }));
    });
    await page.waitForFunction(() => window.reviewFixLoads === 1);
    await context.close();
  }
  {
    const { context, page } = await open('/repositories');
    await page.getByRole('region', { name: 'Connected accounts' }).waitFor();
    const result = await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const { live } = await window.importDashboard('/src/lib/events.ts');
      const original = api.installations;
      window.accountLoads = 0;
      api.installations = () => {
        window.accountLoads++;
        return original();
      };
      for (const login of ['paused-org', 'offline-org']) await api.owner(login, true);
      live.update((s) => ({ ...s, repos: s.repos + 1 }));
      return original();
    });
    await settle(page);
    assert.equal(await page.evaluate(() => window.accountLoads), 0);
    assert.equal(
      result.installations.find((a) => a.account.login === 'paused-org').status,
      'suspended',
    );
    assert.equal(
      result.installations.find((a) => a.account.login === 'offline-org').status,
      'not_installed',
    );
    await context.close();
  }
  {
    const { context, page } = await open(
      '/reviews?status=complete&kind=index&q=missing&repo=northstar/api',
    );
    await page.getByRole('button', { name: 'Review PR URL', exact: true }).click();
    await page
      .getByLabel('GitHub PR URL', { exact: true })
      .fill('https://github.com/community/project/pull/7');
    await page.getByRole('button', { name: 'Queue private review', exact: true }).click();
    await page.getByText('External · private', { exact: true }).waitFor();
    for (const key of ['status', 'kind', 'q', 'repo'])
      assert.equal(new URL(page.url()).searchParams.has(key), false);
    await context.close();
  }
  {
    const { context, page } = await open('/settings?tab=fixes');
    await page.getByRole('switch', { name: 'Enable fix execution', exact: true }).waitFor();
    const checks = await page.evaluate(async () => {
      const { ConfigDocument } = await window.importDashboard('/src/lib/config.svelte.ts');
      const { validFixLimits, fixLimitRanges } =
        await window.importDashboard('/src/lib/fix-policy.ts');
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const invalid = new ConfigDocument('models: [');
      const invalidYaml = invalid.yaml;
      invalid.overrideRoute('fixer');
      const configured = new ConfigDocument(
        'models: {investigator: {model: inherited}, fixer: {model: selected}}',
      );
      configured.overrideRoute('fixer');
      const policy = (await api.fixSettings()).fixes;
      const ranges = Object.entries(fixLimitRanges).every(([key, [min, max]]) => {
        return (
          [min, max].every((v) =>
            validFixLimits({ ...policy, limits: { ...policy.limits, [key]: v } }),
          ) &&
          [min - 1, max + 1, min + 0.5].every(
            (v) => !validFixLimits({ ...policy, limits: { ...policy.limits, [key]: v } }),
          )
        );
      });
      return {
        invalidUnchanged: invalid.yaml === invalidYaml,
        route: configured.get(['models', 'fixer', 'model'], ''),
        ranges,
      };
    });
    assert.deepEqual(checks, { invalidUnchanged: true, route: 'selected', ranges: true });
    await page.getByText('Delivery and requester permissions', { exact: true }).click();
    await page
      .getByRole('switch', { name: 'Require approval before GitHub writes', exact: true })
      .click();
    await page.getByRole('switch', { name: 'Allow commits to the PR branch', exact: true }).click();
    await choose(page, 'Preferred delivery', 'Commit to PR branch');
    await choose(page, 'Preferred delivery', 'Patch preview');
    assert.equal(
      await page
        .getByRole('switch', { name: 'Require approval before GitHub writes', exact: true })
        .getAttribute('aria-checked'),
      'false',
    );
    await page.getByText('Sandbox, tests and resource limits', { exact: true }).click();
    await edit(page, 'fix-limit-model_requests', '1');
    assert.equal(
      await page.getByRole('button', { name: 'Save changes', exact: true }).isEnabled(),
      false,
    );
    await edit(page, 'fix-limit-model_requests', '50');
    await edit(page, 'fix-test-timeout', '3601');
    assert.equal(
      await page.getByRole('button', { name: 'Save changes', exact: true }).isEnabled(),
      false,
    );
    await edit(page, 'fix-test-timeout', '300');
    await page.getByRole('button', { name: 'Save changes', exact: true }).click();
    await page.getByText('Settings saved.', { exact: true }).waitFor();
    // Render a saved command containing escapes, then edit it without changing argv.
    const argv = ['printf', 'a\tb', 'line\nnext', 'quote"slash\\'];
    await page.evaluate(async (argv) => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const data = await api.fixSettings();
      await api.saveFixSettings({ ...data.fixes, test_command: argv });
    }, argv);
    await page.getByRole('link', { name: 'Fixes', exact: true }).click();
    await page.getByRole('link', { name: 'Fix settings', exact: true }).click();
    await page.getByText('Sandbox, tests and resource limits', { exact: true }).click();
    await page
      .getByLabel('Test command (argv)', { exact: true })
      .evaluate((input) => input.dispatchEvent(new Event('input', { bubbles: true })));
    assert.deepEqual(
      await page.locator('.argv code').allTextContents(),
      argv.map((a, i) => `${i}: ${JSON.stringify(a)}`),
    );
    // Save a changed review route, but make only the fix-policy save fail.
    await edit(page, 'fix-limit-max_files', '9');
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      api.saveFixSettings = async () => {
        throw new Error('Fix policy unavailable');
      };
    });
    await page.getByText('Fix model routes', { exact: true }).click();
    await page.getByRole('switch', { name: 'Override fixer model', exact: true }).click();
    await edit(page, 'model-fixer-name', 'partial-save-model');
    await page.getByRole('button', { name: 'Save changes', exact: true }).click();
    await page
      .getByText(
        'Fix policy unavailable Review configuration was saved; fix policy changes are still unsaved.',
        { exact: true },
      )
      .waitFor();
    await context.close();
  }
}
