<script setup lang="ts">
const api = useApi();
const me = useMe();

const tokens = ref<Token[]>([]);
const loading = ref(true);
const showCreate = ref(false);
const newName = ref("");
const created = ref<string | null>(null);
const copied = ref(false);

async function load() {
  loading.value = true;
  try {
    tokens.value = (await api.get<{ tokens: Token[] }>("/api/v1/tokens")).tokens;
  } finally {
    loading.value = false;
  }
}
onMounted(load);

async function create() {
  const res = await api.post<{ token: string }>("/api/v1/tokens", { name: newName.value });
  created.value = res.token;
  newName.value = "";
  await load();
}

async function remove(t: Token) {
  if (!confirm(`Revoke token "${t.name}"?`)) return;
  await api.del(`/api/v1/tokens/${t.id}`);
  await load();
}

function copyToken() {
  if (created.value) navigator.clipboard?.writeText(created.value);
  copied.value = true;
  setTimeout(() => (copied.value = false), 1500);
}

function closeCreate() {
  showCreate.value = false;
  created.value = null;
}
</script>

<template>
  <div>
    <div class="mb-6 flex items-end justify-between">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">Access Tokens</h1>
        <p class="text-sm text-[var(--color-muted)]">Use a token as your password for <code class="text-[var(--color-fg)]">docker login</code>.</p>
      </div>
      <UiButton size="sm" @click="showCreate = true">
        <UiIcon name="plus" :size="14" /> New token
      </UiButton>
    </div>

    <UiCard>
      <div v-if="loading" class="py-8 text-center text-sm text-[var(--color-muted)]">Loading…</div>
      <div v-else-if="!tokens.length" class="py-8 text-center text-sm text-[var(--color-muted)]">No tokens yet.</div>
      <table v-else class="w-full text-sm">
        <thead class="text-left text-xs uppercase tracking-wide text-[var(--color-muted)]">
          <tr class="border-b border-[var(--color-border)]">
            <th class="px-3 py-2 font-medium">Name</th>
            <th class="px-3 py-2 font-medium">Token</th>
            <th class="px-3 py-2 font-medium">Last used</th>
            <th class="px-3 py-2"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="t in tokens" :key="t.id" class="border-b border-[var(--color-border)] last:border-0">
            <td class="px-3 py-3 font-medium">{{ t.name }}</td>
            <td class="px-3 py-3 font-mono text-xs text-[var(--color-muted)]">{{ t.token_prefix }}…</td>
            <td class="px-3 py-3 text-[var(--color-muted)]">{{ t.last_used_at ? timeAgo(t.last_used_at) : "never" }}</td>
            <td class="px-3 py-3 text-right">
              <UiButton variant="ghost" size="sm" @click="remove(t)"><UiIcon name="trash" :size="14" /></UiButton>
            </td>
          </tr>
        </tbody>
      </table>
    </UiCard>

    <UiModal :open="showCreate" :title="created ? 'Token created' : 'New access token'" @close="closeCreate">
      <div v-if="created" class="flex flex-col gap-4">
        <p class="text-sm text-[var(--color-muted)]">Copy it now — you won't be able to see it again.</p>
        <div class="flex items-center gap-2 rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] px-3 py-2 font-mono text-xs">
          <span class="flex-1 break-all">{{ created }}</span>
          <UiButton variant="ghost" size="sm" @click="copyToken">
            <UiIcon :name="copied ? 'check' : 'copy'" :size="14" />
          </UiButton>
        </div>
        <div class="rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] px-3 py-2 font-mono text-xs text-[var(--color-muted)]">
          docker login &lt;host&gt; -u {{ me?.user.username }} -p {{ created }}
        </div>
        <div class="flex justify-end">
          <UiButton @click="closeCreate">Done</UiButton>
        </div>
      </div>
      <form v-else class="flex flex-col gap-4" @submit.prevent="create">
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Name</label>
          <UiInput v-model="newName" placeholder="laptop" required />
        </div>
        <div class="flex justify-end gap-2">
          <UiButton variant="outline" type="button" @click="closeCreate">Cancel</UiButton>
          <UiButton type="submit">Create</UiButton>
        </div>
      </form>
    </UiModal>
  </div>
</template>
