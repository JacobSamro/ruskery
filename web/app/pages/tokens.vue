<script setup lang="ts">
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const api = useApi();
const me = useMe();
const { confirm } = useConfirm();

const tokens = ref<Token[]>([]);
const loading = ref(true);
const showCreate = ref(false);
const newName = ref("");
const created = ref<string | null>(null);
const copied = ref(false);

// Scope selection for a new token.
const orgs = computed(() => me.value?.orgs ?? []);
const scopeType = ref<"all" | "org" | "repo">("all");
const scopeOrg = ref("");
const scopeRepo = ref("");
const scopePerm = ref<"admin" | "push" | "pull">("admin");
const repos = ref<RepoSummary[]>([]);

const PERM_LABEL: Record<string, string> = {
  admin: "full",
  push: "read & write",
  pull: "read-only",
};

watch([scopeType, scopeOrg], async () => {
  if ((scopeType.value === "org" || scopeType.value === "repo") && !scopeOrg.value) {
    scopeOrg.value = orgs.value[0]?.slug ?? "";
  }
  if (scopeType.value === "repo" && scopeOrg.value) {
    repos.value = (
      await api.get<{ repositories: RepoSummary[] }>(`/api/v1/orgs/${scopeOrg.value}/repos`)
    ).repositories;
  }
});

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
  const body: Record<string, string> = { name: newName.value, permission: scopePerm.value };
  if (scopeType.value === "org" && scopeOrg.value) body.org = scopeOrg.value;
  if (scopeType.value === "repo" && scopeOrg.value && scopeRepo.value) {
    body.org = scopeOrg.value;
    body.repo = scopeRepo.value;
  }
  const res = await api.post<{ token: string }>("/api/v1/tokens", body);
  created.value = res.token;
  newName.value = "";
  await load();
}

async function remove(t: Token) {
  if (
    !(await confirm({
      title: "Revoke token",
      message: `Revoke token "${t.name}"? Any client using it will stop working.`,
      confirmText: "Revoke",
      destructive: true,
    }))
  )
    return;
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
  scopeType.value = "all";
  scopeOrg.value = "";
  scopeRepo.value = "";
  scopePerm.value = "admin";
}
</script>

<template>
  <div>
    <div class="mb-6 flex items-end justify-between">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">Access Tokens</h1>
        <p class="text-sm text-muted-foreground">
          Use a token as your password for <code class="text-foreground">docker login</code>.
          Scope a token to one org or repo to limit its reach.
        </p>
      </div>
      <UiButton size="sm" @click="showCreate = true">
        <UiIcon name="plus" :size="14" /> New token
      </UiButton>
    </div>

    <UiCard>
      <div v-if="loading" class="py-8 text-center text-sm text-muted-foreground">Loading…</div>
      <div v-else-if="!tokens.length" class="py-8 text-center text-sm text-muted-foreground">No tokens yet.</div>
      <table v-else class="w-full text-sm">
        <thead class="text-left text-xs uppercase tracking-wide text-muted-foreground">
          <tr class="border-b border-border">
            <th class="px-3 py-2 font-medium">Name</th>
            <th class="px-3 py-2 font-medium">Scope</th>
            <th class="px-3 py-2 font-medium">Permission</th>
            <th class="px-3 py-2 font-medium">Token</th>
            <th class="px-3 py-2 font-medium">Last used</th>
            <th class="px-3 py-2"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="t in tokens" :key="t.id" class="border-b border-border last:border-0">
            <td class="px-3 py-3 font-medium">{{ t.name }}</td>
            <td class="px-3 py-3">
              <UiBadge :variant="t.scope === 'all' ? 'default' : 'outline'">
                {{ t.scope === "all" ? "all access" : t.scope }}
              </UiBadge>
            </td>
            <td class="px-3 py-3 text-muted-foreground">{{ PERM_LABEL[t.max_perm] || t.max_perm }}</td>
            <td class="px-3 py-3 font-mono text-xs text-muted-foreground">{{ t.token_prefix }}…</td>
            <td class="px-3 py-3 text-muted-foreground">{{ t.last_used_at ? timeAgo(t.last_used_at) : "never" }}</td>
            <td class="px-3 py-3 text-right">
              <UiButton variant="ghost" size="sm" @click="remove(t)"><UiIcon name="trash" :size="14" /></UiButton>
            </td>
          </tr>
        </tbody>
      </table>
    </UiCard>

    <UiModal :open="showCreate" :title="created ? 'Token created' : 'New access token'" @close="closeCreate">
      <div v-if="created" class="flex flex-col gap-4">
        <p class="text-sm text-muted-foreground">Copy it now — you won't be able to see it again.</p>
        <div class="flex items-center gap-2 rounded-[var(--radius)] border border-border bg-background px-3 py-2 font-mono text-xs">
          <span class="flex-1 break-all">{{ created }}</span>
          <UiButton variant="ghost" size="sm" @click="copyToken">
            <UiIcon :name="copied ? 'check' : 'copy'" :size="14" />
          </UiButton>
        </div>
        <div class="rounded-[var(--radius)] border border-border bg-background px-3 py-2 font-mono text-xs text-muted-foreground">
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

        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Scope</label>
          <Select v-model="scopeType">
            <SelectTrigger class="w-full" data-testid="token-scope"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All my access</SelectItem>
              <SelectItem value="org">A single organization</SelectItem>
              <SelectItem value="repo">A single repository</SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Permission</label>
          <Select v-model="scopePerm">
            <SelectTrigger class="w-full" data-testid="token-perm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="admin">Full (read, write, delete)</SelectItem>
              <SelectItem value="push">Read &amp; write (pull + push)</SelectItem>
              <SelectItem value="pull">Read-only (pull)</SelectItem>
            </SelectContent>
          </Select>
          <p class="text-xs text-muted-foreground">Caps the token below your own access; never grants more.</p>
        </div>

        <div v-if="scopeType !== 'all'" class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Organization</label>
          <Select v-model="scopeOrg">
            <SelectTrigger class="w-full" data-testid="token-org"><SelectValue placeholder="Select an organization…" /></SelectTrigger>
            <SelectContent>
              <SelectItem v-for="o in orgs" :key="o.slug" :value="o.slug">{{ o.name }}</SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div v-if="scopeType === 'repo'" class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Repository</label>
          <Select v-model="scopeRepo">
            <SelectTrigger class="w-full"><SelectValue placeholder="Select a repository…" /></SelectTrigger>
            <SelectContent>
              <SelectItem v-for="r in repos" :key="r.name" :value="r.name">{{ r.name }}</SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div class="flex justify-end gap-2">
          <UiButton variant="outline" type="button" @click="closeCreate">Cancel</UiButton>
          <UiButton type="submit">Create</UiButton>
        </div>
      </form>
    </UiModal>
  </div>
</template>
