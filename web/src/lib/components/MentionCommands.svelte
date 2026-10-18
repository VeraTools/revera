<script lang="ts">
  let { slug }: { slug: string } = $props();
</script>

<details class="data-disclosure mention-commands">
  <summary>Mention commands</summary>
  <p class="helper">
    In a pull request comment. Only owners, members and collaborators can run commands.
  </p>
  <dl>
    <div>
      <dt><code>@{slug} review</code></dt>
      <dd>Use the repository’s mode</dd>
    </div>
    <div>
      <dt><code>@{slug} quick</code> · <code>@{slug} deep</code></dt>
      <dd>Choose a mode</dd>
    </div>
    <div>
      <dt><code>@{slug} review standard</code></dt>
      <dd>Also accepts quick or deep after review</dd>
    </div>
    <div>
      <dt><code>@{slug} help</code></dt>
      <dd>List commands</dd>
    </div>
  </dl>
</details>

<style>
  .mention-commands {
    margin-top: 28px;
  }
  .helper {
    margin: 12px 0;
  }
  dl {
    font-size: 12px;
  }
  dl > div {
    margin: 12px 0;
  }
  dd {
    margin-top: 4px;
    color: var(--muted);
  }
  code {
    overflow-wrap: anywhere;
  }
</style>
