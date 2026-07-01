<script setup lang="ts">
const route = useRoute();
const api = useApi();
const slug = computed(() => route.params.slug as string);

const stats = ref<OrgStats | null>(null);
const role = ref<string>("");
const repos = ref<RepoSummary[]>([]);
const loading = ref(true);

const canManage = computed(() => role.value === "owner" || role.value === "admin");

async function load() {
  loading.value = true;
  try {
    const [org, list] = await Promise.all([
      api.get<{ role: string; stats: OrgStats }>(`/api/v1/orgs/${slug.value}`),
      api.get<{ repositories: RepoSummary[] }>(`/api/v1/orgs/${slug.value}/repos`),
    ]);
    role.value = org.role;
    stats.value = org.stats;
    repos.value = list.repositories;
  } finally {
    loading.value = false;
  }
  await loadImports();
}
onMounted(load);
watch(slug, load);

// ── create repository ──
const showCreate = ref(false);
const newName = ref("");
const creating = ref(false);
const createError = ref("");

function openCreate() {
  newName.value = "";
  createError.value = "";
  showCreate.value = true;
}
function closeCreate() {
  showCreate.value = false;
}

async function createRepo() {
  const name = newName.value.trim();
  if (!name || creating.value) return;
  creating.value = true;
  createError.value = "";
  try {
    await api.post(`/api/v1/orgs/${slug.value}/repos`, { name });
    closeCreate();
    await load();
  } catch (e) {
    createError.value = apiErrorMessage(e);
  } finally {
    creating.value = false;
  }
}

// ── import from an upstream registry ──
interface ImportJob {
  id: string;
  upstream: string;
  status: "running" | "completed" | "failed";
  repos_total: number;
  repos_done: number;
  tags_total: number;
  tags_done: number;
  blobs_done: number;
  bytes_done: number;
  error: string | null;
  created_at: string;
  updated_at: string;
}

const imports = ref<ImportJob[]>([]);
let pollTimer: ReturnType<typeof setInterval> | null = null;

async function loadImports() {
  if (!canManage.value) return;
  try {
    const res = await api.get<{ imports: ImportJob[] }>(`/api/v1/orgs/${slug.value}/imports`);
    imports.value = res.imports;
  } catch {
    // Non-admins get 403; just leave the panel empty.
    imports.value = [];
  }
  const active = imports.value.some((i) => i.status === "running");
  if (active && !pollTimer) {
    pollTimer = setInterval(loadImports, 2500);
  } else if (!active && pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
    // A run just finished — refresh the repo list so imported repos appear.
    void refreshRepos();
  }
}
onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer);
});

async function refreshRepos() {
  try {
    const list = await api.get<{ repositories: RepoSummary[] }>(
      `/api/v1/orgs/${slug.value}/repos`,
    );
    repos.value = list.repositories;
  } catch {
    /* ignore */
  }
}

const orgs = ref<{ slug: string; name: string; role: string }[]>([]);
const showImport = ref(false);
const importOrg = ref("");
const importHost = ref("");
const importPrefix = ref("");
const importUser = ref("");
const importPass = ref("");
const importing = ref(false);
const importError = ref("");

async function openImport() {
  importHost.value = "";
  importPrefix.value = "";
  importUser.value = "";
  importPass.value = "";
  importError.value = "";
  importOrg.value = slug.value;
  try {
    const res = await api.get<{ orgs: { slug: string; name: string; role: string }[] }>(
      "/api/v1/orgs",
    );
    orgs.value = res.orgs.filter((o) => o.role === "owner" || o.role === "admin");
  } catch {
    orgs.value = [];
  }
  showImport.value = true;
}
function closeImport() {
  showImport.value = false;
}

async function startImport() {
  const host = importHost.value.trim();
  const target = importOrg.value || slug.value;
  if (!host || importing.value) return;
  importing.value = true;
  importError.value = "";
  try {
    await api.post(`/api/v1/orgs/${target}/imports`, {
      host,
      image_prefix: importPrefix.value.trim() || undefined,
      username: importUser.value.trim() || undefined,
      password: importPass.value || undefined,
    });
    closeImport();
    if (target !== slug.value) {
      await navigateTo(`/orgs/${target}`);
    } else {
      await loadImports();
    }
  } catch (e) {
    importError.value = apiErrorMessage(e);
  } finally {
    importing.value = false;
  }
}

function humanBytes(n: number): string {
  if (!n) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i ? 1 : 0)} ${units[i]}`;
}
function importPct(i: ImportJob): number {
  if (i.status === "completed") return 100;
  if (!i.repos_total) return 0;
  return Math.min(99, Math.round((i.repos_done / i.repos_total) * 100));
}
</script>

<template>
  <div>
    <div class="mb-6 flex items-end justify-between gap-4">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">Repositories</h1>
        <p class="text-sm text-muted-foreground">Container images in this organization.</p>
      </div>
      <div class="flex items-center gap-2">
        <template v-if="stats">
          <UiBadge>{{ stats.repos }} repos</UiBadge>
          <UiBadge>{{ stats.members }} members</UiBadge>
          <UiBadge>{{ stats.teams }} teams</UiBadge>
        </template>
        <UiButton
          v-if="canManage"
          size="sm"
          variant="outline"
          data-testid="import-open"
          @click="openImport"
        >
          <UiIcon name="download" :size="16" /> Import
        </UiButton>
        <UiButton v-if="canManage" size="sm" data-testid="new-repo" @click="openCreate">
          <UiIcon name="plus" :size="16" /> New repository
        </UiButton>
      </div>
    </div>

    <!-- import jobs (active + recent) -->
    <UiCard v-if="imports.length" class="mb-4">
      <p class="mb-3 text-sm font-medium">Imports</p>
      <div class="flex flex-col gap-3">
        <div v-for="i in imports" :key="i.id" class="text-sm">
          <div class="flex items-center justify-between gap-2">
            <span class="truncate font-medium">{{ i.upstream }}</span>
            <UiBadge
              :class="{
                'bg-primary/15 text-primary': i.status === 'running',
                'bg-emerald-500/15 text-emerald-600': i.status === 'completed',
                'bg-destructive/15 text-destructive': i.status === 'failed',
              }"
            >
              {{ i.status }}
            </UiBadge>
          </div>
          <div class="mt-1.5 h-1.5 w-full overflow-hidden rounded-full bg-muted">
            <div
              class="h-full rounded-full transition-all"
              :class="i.status === 'failed' ? 'bg-destructive' : 'bg-primary'"
              :style="{ width: `${importPct(i)}%` }"
            />
          </div>
          <div class="mt-1 flex flex-wrap gap-x-4 gap-y-0.5 text-xs text-muted-foreground">
            <span>{{ i.repos_done }}/{{ i.repos_total }} repos</span>
            <span>{{ i.tags_done }}/{{ i.tags_total }} tags</span>
            <span>{{ i.blobs_done }} blobs</span>
            <span>{{ humanBytes(i.bytes_done) }}</span>
          </div>
          <p v-if="i.error" class="mt-1 text-xs text-destructive">{{ i.error }}</p>
        </div>
      </div>
    </UiCard>

    <UiCard>
      <div v-if="loading" class="py-8 text-center text-sm text-muted-foreground">Loading…</div>
      <div v-else-if="!repos.length" class="flex flex-col items-center gap-3 py-12 text-center">
        <UiIcon name="package" :size="32" class="text-muted-foreground" />
        <div>
          <p class="font-medium">No repositories yet</p>
          <p class="mt-1 text-sm text-muted-foreground">
            Create one below, or push your first image:
            <code class="text-foreground">docker push &lt;host&gt;/{{ slug }}/app:latest</code>
          </p>
        </div>
        <UiButton v-if="canManage" size="sm" @click="openCreate">
          <UiIcon name="plus" :size="16" /> New repository
        </UiButton>
      </div>
      <table v-else class="w-full text-sm">
        <thead class="text-left text-xs uppercase tracking-wide text-muted-foreground">
          <tr class="border-b border-border">
            <th class="px-3 py-2 font-medium">Repository</th>
            <th class="px-3 py-2 font-medium">Tags</th>
            <th class="px-3 py-2 font-medium">Updated</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="r in repos"
            :key="r.name"
            class="border-b border-border last:border-0 hover:bg-muted/50"
          >
            <td class="px-3 py-3">
              <NuxtLink
                :to="`/orgs/${slug}/repos/${r.name}`"
                class="flex items-center gap-2 font-medium hover:text-primary"
              >
                <UiIcon name="boxes" :size="16" class="text-muted-foreground" />
                {{ r.name }}
              </NuxtLink>
            </td>
            <td class="px-3 py-3 text-muted-foreground">{{ r.tag_count }}</td>
            <td class="px-3 py-3 text-muted-foreground">{{ timeAgo(r.updated_at) }}</td>
          </tr>
        </tbody>
      </table>
    </UiCard>

    <UiModal :open="showCreate" title="New repository" @close="closeCreate">
      <form class="flex flex-col gap-4" @submit.prevent="createRepo">
        <div>
          <label class="mb-1.5 block text-sm font-medium">Name</label>
          <UiInput v-model="newName" placeholder="team/app" data-testid="new-repo-name" />
          <p class="mt-1.5 text-xs text-muted-foreground">
            Lowercase letters, digits, <code>. _ -</code> and <code>/</code>.
          </p>
        </div>
        <p v-if="createError" class="text-sm text-destructive">{{ createError }}</p>
        <div class="flex justify-end gap-2">
          <UiButton type="button" variant="ghost" @click="closeCreate">Cancel</UiButton>
          <UiButton type="submit" :disabled="creating || !newName.trim()" data-testid="new-repo-submit">
            {{ creating ? "Creating…" : "Create" }}
          </UiButton>
        </div>
      </form>
    </UiModal>

    <UiModal :open="showImport" title="Import from a registry" @close="closeImport">
      <form class="flex flex-col gap-4" @submit.prevent="startImport">
        <p class="text-sm text-muted-foreground">
          Copies <strong>every repository, tag and architecture</strong> from an upstream OCI
          registry that supports catalog listing (registry:2, Harbor, DigitalOcean, GHCR
          Enterprise…). Runs in the background; blobs already present are skipped.
        </p>
        <div>
          <label class="mb-1.5 block text-sm font-medium">Target organization</label>
          <select
            v-model="importOrg"
            data-testid="import-org"
            class="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <option v-for="o in orgs" :key="o.slug" :value="o.slug">
              {{ o.name }} ({{ o.slug }})
            </option>
          </select>
        </div>
        <div>
          <label class="mb-1.5 block text-sm font-medium">Registry host</label>
          <UiInput
            v-model="importHost"
            placeholder="registry.digitalocean.com"
            data-testid="import-host"
          />
          <p class="mt-1.5 text-xs text-muted-foreground">Host or URL (HTTPS assumed).</p>
        </div>
        <div>
          <label class="mb-1.5 block text-sm font-medium">
            Image prefix <span class="font-normal text-muted-foreground">(optional)</span>
          </label>
          <UiInput
            v-model="importPrefix"
            placeholder="your-username / registry-name"
            data-testid="import-prefix"
          />
          <p class="mt-1.5 text-xs text-muted-foreground">
            Only import repositories under this namespace — images are
            <code>registry/prefix/image</code>. For most providers the prefix is your username or
            registry name (e.g. the <code>myuser</code> in
            <code>registry.example.com/myuser/image</code>). Leave empty to import everything, or
            when your registry gives you a dedicated domain.
          </p>
        </div>
        <div class="grid grid-cols-2 gap-3">
          <div>
            <label class="mb-1.5 block text-sm font-medium">Username</label>
            <UiInput v-model="importUser" placeholder="token / user" data-testid="import-user" />
          </div>
          <div>
            <label class="mb-1.5 block text-sm font-medium">Password / token</label>
            <UiInput
              v-model="importPass"
              type="password"
              placeholder="••••••••"
              data-testid="import-pass"
            />
          </div>
        </div>
        <p class="text-xs text-muted-foreground">
          For DigitalOcean, use your API token as both the username and the password.
        </p>
        <p v-if="importError" class="text-sm text-destructive">{{ importError }}</p>
        <div class="flex justify-end gap-2">
          <UiButton type="button" variant="ghost" @click="closeImport">Cancel</UiButton>
          <UiButton
            type="submit"
            :disabled="importing || !importHost.trim()"
            data-testid="import-submit"
          >
            {{ importing ? "Starting…" : "Start import" }}
          </UiButton>
        </div>
      </form>
    </UiModal>
  </div>
</template>
