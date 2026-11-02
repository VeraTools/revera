import assert from 'node:assert/strict';
import { mkdir, readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { chromium } from 'playwright';

async function confirmAction(page, action, accept) {
  const handled = page
    .waitForEvent('dialog', { timeout: 5000 })
    .then((dialog) => (accept ? dialog.accept() : dialog.dismiss()));
  await Promise.all([handled, action()]);
}

export async function checkPlaybook({ open, output }) {
  const shots = join(output, 'playbook');
  await mkdir(shots, { recursive: true });
  const { context, page } = await open('/playbook');
  await page.getByRole('heading', { name: 'Reviewer instructions', exact: true }).waitFor();
  const editor = page.getByLabel('Reviewer markdown', { exact: true });
  const save = page.getByRole('button', { name: 'Save', exact: true });
  const skillEditor = page.getByLabel('Skill markdown', { exact: true });
  const item = (name) => page.getByRole('button', { name, exact: true });
  const book = () =>
    page.evaluate(async () => (await window.importDashboard('/src/lib/api.ts')).api.playbook());
  const initial = await book();
  assert.equal(initial.skills.length, 12);
  assert.equal(
    initial.reviewer.builtin,
    await readFile(new URL('../../playbook/reviewer.md', import.meta.url), 'utf8'),
  );
  for (const skill of initial.skills) {
    assert.equal(
      skill.builtin,
      await readFile(new URL(`../../playbook/skills/${skill.name}.md`, import.meta.url), 'utf8'),
    );
    assert.equal(skill.effective, skill.builtin);
    assert.equal(skill.override, null);
    assert.equal(skill.enabled, true);
  }
  assert.equal(await save.isEnabled(), false);
  await page.screenshot({ path: join(shots, 'desktop.png'), fullPage: true });
  await editor.fill(`${initial.reviewer.effective}\nKeep review scope concrete.\n`);
  await save.click();
  await page.getByText('Reviewer instructions saved.', { exact: true }).waitFor();
  assert.equal(await item('Reviewer instructions').getByText('Edited', { exact: true }).count(), 1);
  const reset = () => page.getByRole('button', { name: 'Reset to built-in', exact: true }).click();
  await confirmAction(page, reset, false);
  assert.notEqual((await book()).reviewer.override, null);
  assert.notEqual(await editor.inputValue(), initial.reviewer.builtin);
  await confirmAction(page, reset, true);
  await page.getByText('Reset to built-in.', { exact: true }).waitFor();
  assert.equal(await editor.inputValue(), initial.reviewer.builtin);

  await item('logic').click();
  const logic = initial.skills.find((skill) => skill.name === 'logic');
  await skillEditor.fill(`${logic.effective}\nCheck a concrete boundary.\n`);
  await save.click();
  await page.getByText('Skill saved.', { exact: true }).waitFor();
  assert.equal(await item('logic').getByText('Edited', { exact: true }).count(), 1);
  assert.notEqual((await book()).digest, initial.digest);
  await page.getByRole('switch', { name: 'Show built-in', exact: true }).click();
  const builtin = page.getByLabel('Built-in logic (read-only)', { exact: true });
  assert.equal(await builtin.inputValue(), logic.builtin);
  assert.equal(await builtin.getAttribute('readonly'), '');
  await page.screenshot({ path: join(shots, 'edited-comparison.png'), fullPage: true });
  await confirmAction(page, reset, false);
  assert.notEqual((await book()).skills.find((skill) => skill.name === 'logic').override, null);
  await confirmAction(page, reset, true);
  await page.getByText('Reset to built-in.', { exact: true }).waitFor();
  assert.equal(await skillEditor.inputValue(), logic.builtin);
  assert.equal(await item('logic').getByText('Edited', { exact: true }).count(), 0);
  assert.equal((await book()).digest, initial.digest);

  await page.getByRole('switch', { name: 'Enable logic', exact: true }).click();
  await page.getByText('Skill disabled.', { exact: true }).waitFor();
  assert.equal(await item('logic').getByText('Disabled', { exact: true }).count(), 1);
  const disabled = (await book()).skills.find((skill) => skill.name === 'logic');
  assert.equal(disabled.enabled, false);
  assert.ok(disabled.effective.includes('enabled: false'));
  await page.screenshot({ path: join(shots, 'disabled.png'), fullPage: true });
  await page.getByRole('switch', { name: 'Enable logic', exact: true }).click();
  await page.getByText('Skill enabled.', { exact: true }).waitFor();
  assert.equal(await item('logic').getByText('Disabled', { exact: true }).count(), 0);

  await skillEditor.fill('x'.repeat(initial.limits.skill_max_bytes + 1));
  assert.equal(await save.isEnabled(), false);
  await page
    .getByText('Over size limit. Shorten the text before saving.', { exact: true })
    .waitFor();
  // Count UTF-8 bytes, not JavaScript string length.
  await skillEditor.fill('界'.repeat(3000));
  await page.getByText(`9000 / ${initial.limits.skill_max_bytes} bytes`, { exact: true }).waitFor();
  assert.equal(await save.isEnabled(), false);
  await page.screenshot({ path: join(shots, 'over-limit.png'), fullPage: true });
  await page.getByRole('button', { name: 'Discard', exact: true }).click();
  await skillEditor.fill(logic.effective.replace('name: logic', 'name: contracts'));
  await save.click();
  await page.getByRole('alert').filter({ hasText: 'Name mismatch' }).waitFor();
  await skillEditor.fill('not front matter');
  await save.click();
  await page.getByRole('alert').filter({ hasText: 'Bad front matter' }).waitFor();
  await page.evaluate(async () => {
    const { api } = await window.importDashboard('/src/lib/api.ts');
    await api.setSecret('PLAYBOOK_TEST_KEY', 'fixture-playbook-secret');
  });
  await skillEditor.fill(`${logic.effective}\nfixture-playbook-secret\n`);
  await save.click();
  await page.getByRole('alert').filter({ hasText: 'contains a registered secret' }).waitFor();
  await page.getByRole('button', { name: 'Discard', exact: true }).click();

  await skillEditor.fill(`${logic.effective}\nUnsaved edit\n`);
  await confirmAction(page, () => item('contracts').click(), false);
  assert.ok((await skillEditor.inputValue()).includes('Unsaved edit'));
  assert.equal(await item('logic').getAttribute('aria-pressed'), 'true');
  await confirmAction(
    page,
    () => page.getByRole('link', { name: 'Settings', exact: true }).click(),
    false,
  );
  assert.equal(new URL(page.url()).pathname, '/playbook');
  const unloadGuarded = await page.evaluate(() => {
    const event = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(event);
    return event.defaultPrevented;
  });
  assert.equal(unloadGuarded, true);
  await confirmAction(page, () => item('contracts').click(), true);
  assert.equal(
    await skillEditor.inputValue(),
    initial.skills.find((skill) => skill.name === 'contracts').effective,
  );

  await page.getByRole('button', { name: 'New skill', exact: true }).click();
  const template = await skillEditor.inputValue();
  for (const section of ['Applies to', 'Check', 'Evidence', 'Not a defect'])
    assert.ok(template.includes(`## ${section}`));
  await skillEditor.fill(template.replace('name: my-skill', 'name: browser-check'));
  await save.click();
  await page.getByText('Skill saved.', { exact: true }).waitFor();
  assert.equal(await item('browser-check').getByText('Custom', { exact: true }).count(), 1);
  assert.equal((await book()).skills.find((skill) => skill.name === 'browser-check').custom, true);
  await confirmAction(
    page,
    () => page.getByRole('button', { name: 'Delete', exact: true }).click(),
    false,
  );
  assert.equal(await item('browser-check').count(), 1);
  await confirmAction(
    page,
    () => page.getByRole('button', { name: 'Delete', exact: true }).click(),
    true,
  );
  await page.getByText('Custom skill deleted.', { exact: true }).waitFor();
  assert.equal(await item('browser-check').count(), 0);
  assert.equal((await book()).skills.length, initial.skills.length);

  // Validate the same limits directly at the mock API, including atomic failures.
  const validation = await page.evaluate(async () => {
    const { api } = await window.importDashboard('/src/lib/api.ts');
    const failures = [];
    for (const [name, text] of [
      ['logic', 'x'.repeat(8193)],
      ['../escape', 'bad'],
      ['logic', '---\nname: logic\ndescription: Test\nunknown: yes\n---\nBody'],
      ['logic', "---\nname: logic\ndescription: ''\n---\nBody"],
      ['logic', "---\nname: logic\ndescription: '   '\n---\nBody"],
    ]) {
      try {
        await api.savePlaybookSkill(name, text);
      } catch (error) {
        failures.push(error.status);
      }
    }
    // Match the server's acceptance of trailing whitespace on front-matter fences.
    await api.savePlaybookSkill('logic', '---\nname: logic\ndescription: Test\n--- \t\nBody');
    await api.deletePlaybookSkill('logic');
    for (let i = 0; i < 12; i++)
      await api.savePlaybookSkill(
        `custom-${i}`,
        `---\nname: custom-${i}\ndescription: Test\n---\nBody`,
      );
    try {
      await api.savePlaybookSkill(
        'one-too-many',
        '---\nname: one-too-many\ndescription: Test\n---\nBody',
      );
    } catch (error) {
      failures.push(error.status);
    }
    for (let i = 0; i < 12; i++) await api.deletePlaybookSkill(`custom-${i}`);
    const before = await api.playbook();
    let totalRejected = false;
    for (const skill of before.skills) {
      const header = `---\nname: ${skill.name}\ndescription: Test\n---\n`;
      try {
        await api.savePlaybookSkill(skill.name, header + 'x'.repeat(8192 - header.length));
      } catch (error) {
        totalRejected = error.status === 400 && error.message.includes('total');
        break;
      }
    }
    for (const skill of before.skills) await api.deletePlaybookSkill(skill.name);
    return { failures, totalRejected, after: await api.playbook() };
  });
  assert.deepEqual(validation.failures, [400, 400, 400, 400, 400, 400]);
  assert.equal(validation.totalRejected, true);
  assert.equal(validation.after.digest, initial.digest);
  await context.close();

  for (const scheme of ['light', 'dark']) {
    const { context, page } = await open('/playbook', scheme, { width: 390, height: 844 });
    // Exercise both ends and the middle of the scroll box. The selected border
    // must fit inside it, while New skill remains outside and unobscured.
    for (const name of ['logic', 'docs', 'security']) {
      await page.getByRole('button', { name, exact: true }).click();
      await page.getByRole('heading', { name, exact: true }).waitFor();
      const scrollBox = await page.evaluate(() => {
        const card = document.querySelector('.playbook-list');
        const heading = card.querySelector('.list-heading');
        const scroller = card.querySelector('.skill-list');
        const selected = scroller.querySelector('.selected').getBoundingClientRect();
        const box = scroller.getBoundingClientRect();
        const button = card.querySelector('.new-skill');
        const newSkill = button.getBoundingClientRect();
        return {
          opaqueHeader: getComputedStyle(heading).backgroundColor !== 'rgba(0, 0, 0, 0)',
          matchesCard:
            getComputedStyle(heading).backgroundColor === getComputedStyle(card).backgroundColor,
          separateHeader: heading.getBoundingClientRect().bottom < box.top,
          borderVisible:
            selected.top >= box.top + 1 &&
            selected.bottom <= box.bottom - 1 &&
            selected.left >= box.left + 1 &&
            selected.right <= box.right - 1,
          newSkillOutside:
            newSkill.top >= box.bottom + 8 && newSkill.bottom < card.getBoundingClientRect().bottom,
          newSkillVisible: button.contains(
            document.elementFromPoint(
              newSkill.left + newSkill.width / 2,
              newSkill.top + newSkill.height / 2,
            ),
          ),
        };
      });
      assert.deepEqual(
        scrollBox,
        {
          opaqueHeader: true,
          matchesCard: true,
          separateHeader: true,
          borderVisible: true,
          newSkillOutside: true,
          newSkillVisible: true,
        },
        `${scheme}: ${name} scroll-box layout`,
      );
    }
    const layout = await page.evaluate(() => {
      const list = document.querySelector('.playbook-list').getBoundingClientRect();
      const editor = document.querySelector('.playbook-editor').getBoundingClientRect();
      return {
        stacked: list.bottom <= editor.top,
        fits: document.documentElement.scrollWidth <= innerWidth,
      };
    });
    assert.deepEqual(layout, { stacked: true, fits: true });
    await page.screenshot({ path: join(shots, `mobile-${scheme}.png`), fullPage: true });
    await context.close();
  }
  {
    const { context, page } = await open('/reviews/review-105');
    await page
      .locator('.badge')
      .filter({ hasText: /^state$/ })
      .waitFor();
    await page
      .locator('.badge')
      .filter({ hasText: /^concurrency$/ })
      .waitFor();
    const pull = await page.evaluate(async () =>
      (await window.importDashboard('/src/lib/api.ts')).api.pull('northstar', 'api', 254),
    );
    assert.equal(pull.review.state, 'findings');
    assert.equal(pull.review.latest.base_ref, pull.base_ref);
    await context.close();
  }
}

// Same Chromium, environment overrides and module-instance setup as browser-check.mjs.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const browser = await chromium.launch({
    executablePath:
      process.env.CHROMIUM_PATH ||
      join(homedir(), '.cache/ms-playwright/chromium-1247/chrome-linux64/chrome'),
    headless: true,
  });
  const errors = [];
  try {
    await checkPlaybook({
      output: process.env.REVERA_WEB_OUTPUT || join(homedir(), '.local/tmp/revera-shots'),
      open: async (path, colorScheme = 'light', viewport = { width: 1440, height: 1000 }) => {
        const context = await browser.newContext({
          viewport,
          colorScheme,
          reducedMotion: 'reduce',
        });
        const page = await context.newPage();
        await page.addInitScript(() => {
          window.importDashboard = (path) => {
            const loaded = performance
              .getEntriesByType('resource')
              .map((entry) => entry.name)
              .findLast((name) => new URL(name).pathname === path);
            return import(loaded || path);
          };
        });
        page.on('pageerror', (error) => errors.push(error.message));
        await page.goto(`${process.env.REVERA_WEB_URL || 'http://127.0.0.1:5173'}${path}`);
        await page.waitForFunction(() => !document.body.innerText.includes('Loading '));
        return { context, page };
      },
    });
    assert.deepEqual(errors, [], 'No browser runtime errors');
    console.log(
      'PASS: playbook defaults, editing/reset, enable/disable, custom create/delete, limits, validation, dirty guards, category chips and 390px layout.',
    );
  } finally {
    await browser.close();
  }
}
