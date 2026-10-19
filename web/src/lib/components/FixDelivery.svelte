<script lang="ts">
  import type { FixCapability, FixDelivery } from '#lib/types';
  let {
    capabilities,
    value = $bindable<FixDelivery>('patch'),
    selectable = false,
  }: { capabilities: FixCapability[]; value?: FixDelivery; selectable?: boolean } = $props();
  const labels = {
    patch: 'Patch preview',
    draft_pr: 'Draft pull request',
    same_branch: 'Commit to PR branch',
  };
</script>

<fieldset class="delivery-options">
  <legend>{selectable ? 'Delivery method' : 'Available delivery methods'}</legend>
  {#each capabilities as capability}
    <div class="delivery-option">
      {#if selectable}<input
          type="radio"
          name="fix-delivery"
          id={`delivery-${capability.mode}`}
          value={capability.mode}
          bind:group={value}
          disabled={!capability.available}
          aria-describedby={`reason-${capability.mode}`}
        />{/if}
      <div>
        {#if selectable}<label for={`delivery-${capability.mode}`}>{labels[capability.mode]}</label
          >{:else}<strong>{labels[capability.mode]}</strong>{/if}
        <p class="helper" id={`reason-${capability.mode}`}>
          {capability.available
            ? capability.mode === 'patch'
              ? 'Download only. No GitHub write.'
              : 'Available. Publication rechecks permissions and the PR head.'
            : capability.reason}
        </p>
        {#if !capability.available && capability.reason.includes('Contents: write')}<a
            class="helper text-link"
            href="/repositories">Update app permissions</a
          >{/if}
      </div>
    </div>
  {/each}
</fieldset>

<style>
  .delivery-options {
    border: 0;
    padding: 0;
    margin: 20px 0;
    min-width: 0;
  }
  legend {
    font-size: 13px;
    font-weight: 600;
    margin-bottom: 8px;
  }
  .delivery-option {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 0;
  }
  input {
    width: 14px;
    min-height: 14px;
    height: 14px;
    margin-top: 4px;
    accent-color: var(--accent);
  }
  label,
  strong {
    font-size: 12px;
    font-weight: 500;
  }
</style>
