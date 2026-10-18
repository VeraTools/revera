export function createNotices() {
  const state = $state({ error: '', message: '' });
  let sequence = $state(0);
  $effect(() => {
    sequence;
    if (!state.message) return;
    const timer = setTimeout(() => (state.message = ''), 5000);
    return () => clearTimeout(timer);
  });
  return {
    state,
    success(message: string) {
      state.error = '';
      state.message = message;
      sequence++;
    },
    failure(error: string) {
      state.message = '';
      state.error = error;
    },
  };
}
