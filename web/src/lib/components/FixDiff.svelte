<script lang="ts">
  import type { FixRecord } from '#lib/types';
  let { patch, files }: { patch: string; files: NonNullable<FixRecord['patch']>['files'] } =
    $props();
  const blocks = $derived(
    patch
      .split(/(?=^diff --git )/m)
      .filter(Boolean)
      .map((text, index) => {
        const path = text.match(/^\+\+\+ b\/(.+)$/m)?.[1] || text.match(/^--- a\/(.+)$/m)?.[1];
        const file = files.find((f) => f.path === path) || files[index];
        return { text, file, path: path || file?.path || 'Patch' };
      }),
  );
</script>

<section class="section" aria-label="Patch diff">
  <div class="section-heading">
    <h2>Changes <span class="helper">{files.length} files</span></h2>
    <span class="helper">Unified diff · reviewed SHA</span>
  </div>
  {#each blocks as block}<details class="diff-file" open>
      <summary
        ><span class="mono">{block.path}</span>{#if block.file}<span class="file-stats"
            ><span class="muted">{block.file.status}</span>
            <span class="added">+{block.file.additions}</span>
            <span class="removed">−{block.file.deletions}</span></span
          >{/if}</summary
      >
      <pre aria-label={`Diff for ${block.path}`}>{#each block.text
          .trimEnd()
          .split('\n') as line}<span
            class:add={line.startsWith('+') && !line.startsWith('+++')}
            class:remove={line.startsWith('-') && !line.startsWith('---')}
            class:hunk={line.startsWith('@@')}>{line || ' '}{'\n'}</span
          >{/each}</pre>
    </details>{/each}
</section>

<style>
  .diff-file {
    border: 1px solid var(--border);
    border-radius: 6px;
    margin: 12px 0;
    overflow: hidden;
  }
  summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 8px;
    padding: 12px 16px;
    background: var(--surface);
    cursor: pointer;
    font-size: 12px;
  }
  summary .mono {
    overflow-wrap: anywhere;
  }
  .file-stats {
    display: flex;
    gap: 10px;
    font-size: 11px;
  }
  .added {
    color: var(--green);
  }
  .removed {
    color: var(--red);
  }
  pre {
    margin: 0;
    padding: 12px 0;
    overflow-x: auto;
    font-size: 11px;
    line-height: 1.9;
    border-top: 1px solid var(--border);
  }
  pre span {
    display: block;
    padding: 0 16px;
    min-width: max-content;
    white-space: pre;
  }
  .add {
    color: var(--green);
    background: light-dark(#f0fdf4, #172c20);
  }
  .remove {
    color: var(--red);
    background: light-dark(#fef2f2, #321f21);
  }
  .hunk {
    color: var(--blue);
    background: light-dark(#eff6ff, #1b2535);
  }
</style>
