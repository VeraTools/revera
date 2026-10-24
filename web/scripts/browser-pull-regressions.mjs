import assert from 'node:assert/strict';
import { join } from 'node:path';

const settle = (page) =>
  page.evaluate(
    () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
  );

export async function checkPullRegressions({ open, output }) {
  {
    const { context, page } = await open('/pulls/northstar/api/254');
    await page.getByText('2 still open from earlier reviews', { exact: true }).waitFor();
    assert.equal(await page.getByText('2 findings', { exact: true }).count(), 1);
    assert.equal(await page.getByRole('link', { name: 'Fix findings', exact: true }).count(), 2);
    const carried = await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const detail = await api.job('pull-review-254');
      const plan = await api.fixPlan('pull-review-254');
      const pull = await api.pull('northstar', 'api', 254);
      return {
        accepted: detail.findings.length,
        carried: detail.carried.length,
        fixable: plan.findings.length,
        commentUrls: [...pull.comments, ...pull.threads.flatMap((thread) => thread.comments)].every(
          (comment) => comment.url === `${pull.url}#${comment.id}`,
        ),
      };
    });
    assert.deepEqual(carried, { accepted: 0, carried: 2, fixable: 2, commentUrls: true });
    if (output) await page.screenshot({ path: join(output, 'carried.png'), fullPage: true });

    const checks = await page.evaluate(async () => {
      const { renderGithub } = await window.importDashboard('/src/lib/markdown.ts');
      const parse = (body) => new DOMParser().parseFromString(renderGithub(body), 'text/html');
      const results = {};
      let doc = parse('[normal **label**](https://example.test/path?q=1&next=2)');
      const link = doc.querySelector('a');
      results.normal =
        link?.textContent === 'normal label' &&
        link.getAttribute('href') === 'https://example.test/path?q=1&next=2' &&
        link.getAttribute('target') === '_blank' &&
        link.getAttribute('rel') === 'noreferrer';
      for (const label of ['https://a.com', '**https://a.com**', '_https://a.com_']) {
        const html = renderGithub(`[${label}](https://a.com)`);
        doc = new DOMParser().parseFromString(html, 'text/html');
        results[`urlLabel:${label}`] =
          (html.match(/<a\s/g) || []).length === 1 &&
          doc.querySelectorAll('a').length === 1 &&
          doc.querySelector('a')?.textContent === 'https://a.com' &&
          doc.querySelector('a').getAttribute('href') === 'https://a.com';
      }
      doc = parse('[outer [inner](https://inner.test)](https://outer.test)');
      results.nested =
        doc.querySelectorAll('a').length === 2 &&
        doc.querySelector('a').textContent === 'inner' &&
        doc.querySelectorAll('a')[1].getAttribute('href') === 'https://outer.test' &&
        doc.body.textContent === '[outer inner](https://outer.test)' &&
        !doc.querySelector('a a');
      doc = parse(
        '[quoted](https://example.test/"onmouseover="alert)\nhttps://example.test/\"onclick=\"alert',
      );
      results.quoted =
        doc.querySelectorAll('a').length === 2 &&
        !doc.querySelector('[onclick],[onmouseover]') &&
        [...doc.querySelectorAll('a')].every((a) => a.protocol === 'https:');
      doc = parse('[script](javascript:alert(1)) [data](data:text/html,<script>alert(1)</script>)');
      results.schemes = !doc.querySelector('a,script');
      doc = parse(
        '&lt;img src=x onerror=alert(1)&gt; [entity](https://example.test/?q=&quot;bad&amp;x=1)',
      );
      results.entities =
        !doc.querySelector('img,[onerror]') &&
        doc.body.textContent.includes('&lt;img') &&
        doc.querySelector('a').getAttribute('href') === 'https://example.test/?q=&quot;bad&amp;x=1';
      doc = parse('```html\n<!--\n<img src=x>\n```\n\nAfter the fence');
      results.fence =
        doc.querySelector('pre code')?.textContent === '<!--\n<img src=x>' &&
        doc.body.textContent.includes('After the fence') &&
        !doc.querySelector('img');
      doc = parse(
        'Literal `<!--` and `<!-- keep -->` survive.\n\nAfter inline code<!-- hidden\nmore hidden --> visible<!-- unfinished',
      );
      results.inline =
        doc.querySelectorAll('code').length === 2 &&
        doc.body.textContent.includes('<!-- keep -->') &&
        doc.body.textContent.includes('After inline code visible') &&
        !doc.body.textContent.includes('hidden') &&
        !doc.body.textContent.includes('unfinished');
      doc = parse(
        '<!-- hidden\n```\n-->\nVisible\n\n<details><summary>Safe <b>summary</b></summary>\n\nBody\n\n</details>',
      );
      results.details =
        doc.querySelectorAll('details').length === 1 &&
        doc.querySelector('summary')?.textContent === 'Safe summary' &&
        doc.querySelector('details p')?.textContent === 'Body' &&
        !doc.querySelector('b,pre') &&
        doc.body.textContent.includes('Visible');
      const liveBody = document.createElement('div');
      liveBody.innerHTML = renderGithub(
        '<img id="injected-image" src=x onerror="window.__pullXss=true">\n<script>window.__pullXss=true</script>\n[unsafe](javascript:window.__pullXss=true)\n![image](https://example.test/no-load.png)',
      );
      document.body.append(liveBody);
      try {
        await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        results.xss =
          !liveBody.querySelector('img,script,[onerror],#injected-image') &&
          ![...liveBody.querySelectorAll('a')].some((link) => link.protocol === 'javascript:') &&
          liveBody.querySelector('a')?.textContent === 'image' &&
          window.__pullXss === undefined;
      } finally {
        liveBody.remove();
      }
      const brackets = '['.repeat(65000);
      const start = performance.now();
      const html = renderGithub(brackets);
      results.linear = performance.now() - start < 1000;
      results.brackets =
        new DOMParser().parseFromString(html, 'text/html').body.textContent.trim() === brackets;
      return results;
    });
    for (const [name, passed] of Object.entries(checks))
      assert.equal(passed, true, `GitHub markdown: ${name}`);

    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const original = api.pull;
      window.originalPull = await original('northstar', 'api', 254);
      window.pullLoads = 0;
      window.pullOverride = {};
      api.pull = async (...args) => {
        window.pullLoads++;
        return { ...(await original(...args)), ...window.pullOverride };
      };
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, job: { repo_id: 1, pr: 254 } }));
    });
    await settle(page);
    await page.evaluate(async () => {
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, job: { repo_id: 2, pr: 999 } }));
    });
    await page.waitForFunction(() => window.pullLoads === 1);
    await page.evaluate(async () => {
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, job: { repo_id: 2, pr: 998 } }));
    });
    await page.waitForTimeout(650); // An unrelated event must not schedule another refresh.
    assert.equal(await page.evaluate(() => window.pullLoads), 1);
    await page.evaluate(async () => {
      const { live } = await window.importDashboard('/src/lib/events.ts');
      window.pullOverride = { trigger: 'off' };
      live.update((s) => ({ ...s, repos: s.repos + 1 }));
    });
    await page.waitForFunction(() => window.pullLoads === 2);
    await page.getByRole('button', { name: 'Actions for northstar/api #254' }).click();
    const quick = page.getByRole('menuitem', { name: 'Quick', exact: true });
    await quick.waitFor();
    assert.equal(await quick.getAttribute('data-disabled'), '');
    await page.getByText('Reviews are off for this repository', { exact: true }).waitFor();
    await page.keyboard.press('Escape');
    for (const [state, merged] of [
      ['closed', false],
      ['open', true],
    ]) {
      await page.evaluate(
        async ({ state, merged }) => {
          window.pullOverride = { state, merged };
          const { live } = await window.importDashboard('/src/lib/events.ts');
          live.update((s) => ({ ...s, repos: s.repos + 1 }));
        },
        { state, merged },
      );
      await page
        .locator('.pull-actions')
        .getByText(merged ? 'Merged' : 'Closed', { exact: true })
        .waitFor();
      assert.equal(await page.getByRole('link', { name: 'Fix findings', exact: true }).count(), 0);
      assert.equal(await page.getByRole('button', { name: 'Review', exact: true }).count(), 0);
      await page.getByRole('button', { name: 'Actions for northstar/api #254' }).click();
      assert.equal(
        await page.getByRole('menuitem', { name: /^(Quick|Standard|Deep)$/ }).count(),
        0,
      );
      await page.keyboard.press('Escape');
      await page.evaluate(async () => {
        const { api } = await window.importDashboard('/src/lib/api.ts');
        api.cancel = async (id) => {
          window.cancelledReview = id;
          window.pullOverride.review = window.originalPull.review;
        };
        window.cancelledReview = null;
        window.pullOverride.review = {
          ...window.originalPull.review,
          state: 'reviewing',
          active_job: 'active-review-254',
        };
        const { live } = await window.importDashboard('/src/lib/events.ts');
        live.update((s) => ({ ...s, repos: s.repos + 1 }));
      });
      const cancel = page.getByRole('button', { name: 'Cancel', exact: true });
      await cancel.waitFor();
      assert.equal(await page.getByRole('link', { name: 'Fix findings', exact: true }).count(), 0);
      assert.equal(await page.getByRole('button', { name: 'Review', exact: true }).count(), 0);
      await page.getByRole('button', { name: 'Actions for northstar/api #254' }).click();
      assert.equal(
        await page.getByRole('menuitem', { name: /^(Quick|Standard|Deep)$/ }).count(),
        0,
      );
      await page.keyboard.press('Escape');
      await cancel.click();
      await page
        .locator('.pull-actions')
        .getByText(merged ? 'Merged' : 'Closed', { exact: true })
        .waitFor();
      assert.equal(await page.evaluate(() => window.cancelledReview), 'active-review-254');
    }
    await page.evaluate(async () => {
      const pull = window.originalPull;
      window.pullOverride = {
        comments: pull.comments.slice(0, 1),
        reviews: pull.reviews.slice(0, 1),
        threads: pull.threads.slice(0, 1),
        truncated: { labels: true, comments: false, reviews: false, threads: false },
      };
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, repos: s.repos + 1 }));
    });
    await page
      .getByText('1 comment · 1 review · 1 thread (0 unresolved)', { exact: true })
      .waitFor();
    assert.equal(
      await page.getByText('Some conversation items are not shown.', { exact: false }).count(),
      0,
    );
    await page.evaluate(async () => {
      window.pullOverride.truncated.comments = true;
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, repos: s.repos + 1 }));
    });
    await page.getByText('Some conversation items are not shown.', { exact: false }).waitFor();
    await page.evaluate(async () => {
      window.pullLoads = 0;
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({ ...s, job: { repo_id: 1, pr: 254 } }));
    });
    await settle(page);
    await page.locator('.pull-breadcrumb a').click();
    await page.locator('.pull-row').last().waitFor();
    await page.waitForTimeout(650); // Wait beyond the pending timer to prove route cleanup cancels it.
    assert.equal(await page.evaluate(() => window.pullLoads), 0);
    const carriedRow = page.locator('[data-pull="northstar/api#254"]');
    await carriedRow.getByText('2 findings', { exact: true }).waitFor();
    assert.equal(
      await carriedRow.getByRole('link', { name: 'Fix findings', exact: true }).count(),
      1,
    );
    await context.close();
  }
  for (const event of ['job', 'fix']) {
    const { context, page } = await open('/pulls');
    await page.locator('.pull-row').last().waitFor();
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const original = api.pull;
      window.pullLoads = 0;
      api.pull = async (...args) => {
        window.pullLoads++;
        const result = await original(...args);
        if (window.pullLoads === 1) {
          await new Promise((resolve) => {
            window.releasePull = resolve;
          });
        }
        return result;
      };
    });
    await page.locator('[data-pull="northstar/api#254"] .row-title').click();
    await page.waitForFunction(() => typeof window.releasePull === 'function');
    await page.evaluate(async (event) => {
      const { live } = await window.importDashboard('/src/lib/events.ts');
      live.update((s) => ({
        ...s,
        [event]:
          event === 'job'
            ? { repo_id: 1, pr: 254 }
            : { repo_source: 'installed', repo: 'northstar/api', pr: 254 },
      }));
    }, event);
    await settle(page);
    assert.equal(await page.evaluate(() => window.pullLoads), 1);
    await page.evaluate(() => window.releasePull());
    await page.getByText('2 still open from earlier reviews', { exact: true }).waitFor();
    await page.waitForFunction(() => window.pullLoads === 2);
    await page.waitForTimeout(650); // Loading the refreshed detail must not schedule a third request.
    assert.equal(await page.evaluate(() => window.pullLoads), 2, `${event} during initial load`);
    await context.close();
  }
  {
    const { context, page } = await open('/repositories');
    await page.evaluate(async () => {
      const { api } = await window.importDashboard('/src/lib/api.ts');
      const original = api.repoGuidance;
      window.guidanceLimit = 64;
      window.guidanceOmitted = 3;
      api.repoGuidance = async (...args) => {
        const result = await original(...args);
        if (result.files_omitted !== 0) throw new Error('Expected no omitted mock guidance files.');
        return {
          ...result,
          repo: { ...result.repo, guidance: 'agents' },
          max_bytes: window.guidanceLimit,
          files_omitted: window.guidanceOmitted,
          files: result.files.map((file) => (!file.dir ? { ...file, bytes: 50 } : file)),
        };
      };
      const settings = api.settings;
      api.settings = async () => {
        const result = await settings();
        return {
          ...result,
          yaml: result.yaml.replace(
            /^review:\s*$/m,
            `review:\n  guidance: agents\n  guidance_max_bytes: ${window.guidanceLimit}`,
          ),
          server: {
            ...result.server,
            repos: { ...result.server.repos, 'northstar/api': { guidance: 'agents' } },
          },
        };
      };
    });
    await page.getByRole('button', { name: 'Actions for northstar/api', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Review guidance…', exact: true }).click();
    await page
      .getByText('Root files come first; nested files share the remaining 64 B budget.', {
        exact: true,
      })
      .waitFor();
    await page.getByText('Root guidance will be truncated to 64 B.', { exact: true }).waitFor();
    assert.equal(await page.getByText('Will be read', { exact: true }).count(), 0);
    assert.ok((await page.getByText('Eligible', { exact: true }).count()) > 0);
    await page.getByText('3 more guidance files are not shown.', { exact: true }).waitFor();
    await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click();
    await page.evaluate(() => {
      window.guidanceLimit = 50 + new TextEncoder().encode('\n--- AGENTS.md ---\n').length;
      window.guidanceOmitted = 0;
    });
    await page.getByRole('button', { name: 'Actions for northstar/api', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Review guidance…', exact: true }).click();
    await page
      .getByText('Root files come first; nested files share the remaining 69 B budget.', {
        exact: true,
      })
      .waitFor();
    await page.getByRole('button', { name: 'AGENTS.md 50 B', exact: true }).waitFor();
    assert.equal(
      await page.getByText('Root guidance will be truncated', { exact: false }).count(),
      0,
    );
    assert.equal(
      await page.getByText('more guidance files are not shown.', { exact: false }).count(),
      0,
    );
    await context.close();
  }
  console.log(
    'PASS: pull carried/closed cancellation, conversation counts/URLs/truncation, live refresh during load/retention/repos/route cleanup, live GitHub markdown safety/linear parsing and guidance budget/omission preview.',
  );
}
