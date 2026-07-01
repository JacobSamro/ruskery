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
const importProvider = ref("generic");
const importHost = ref("");
const importPrefix = ref("");
const importUser = ref("");
const importPass = ref("");
const importNamespace = ref("");
const importNamespaces = ref<{ name: string; repo_count: number | null }[]>([]);
const discovering = ref(false);
const discoverError = ref("");
const importing = ref(false);
const importError = ref("");

// DigitalOcean uses a single API token for both fields; other providers use a
// username + password/token pair.
function importCreds(): { username?: string; password?: string } {
  const password = importPass.value.trim();
  const username =
    importProvider.value === "digitalocean" ? password : importUser.value.trim();
  return { username: username || undefined, password: password || undefined };
}

const canStartImport = computed(() => {
  if (importProvider.value === "generic") return !!importHost.value.trim();
  return !!importNamespace.value; // DO/GitHub require a picked namespace
});

async function openImport() {
  importProvider.value = "generic";
  importHost.value = "";
  importPrefix.value = "";
  importUser.value = "";
  importPass.value = "";
  importNamespace.value = "";
  importNamespaces.value = [];
  discoverError.value = "";
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

// Switching provider invalidates any loaded namespace list.
function onProviderChange() {
  importNamespace.value = "";
  importNamespaces.value = [];
  discoverError.value = "";
}

// Load the provider's registries/owners into the dropdown from its API.
async function discoverNamespaces() {
  discoverError.value = "";
  importNamespaces.value = [];
  importNamespace.value = "";
  const creds = importCreds();
  if (!creds.password) {
    discoverError.value = "Enter your token first.";
    return;
  }
  discovering.value = true;
  try {
    const res = await api.post<{
      namespaces: { name: string; repo_count: number | null }[];
    }>(`/api/v1/orgs/${importOrg.value || slug.value}/imports/discover`, {
      provider: importProvider.value,
      ...creds,
    });
    importNamespaces.value = res.namespaces;
    if (res.namespaces.length) importNamespace.value = res.namespaces[0]!.name;
    else discoverError.value = "Nothing found for this token.";
  } catch (e) {
    discoverError.value = apiErrorMessage(e);
  } finally {
    discovering.value = false;
  }
}

async function startImport() {
  const target = importOrg.value || slug.value;
  if (importing.value || !canStartImport.value) return;
  const isGeneric = importProvider.value === "generic";
  importing.value = true;
  importError.value = "";
  try {
    await api.post(`/api/v1/orgs/${target}/imports`, {
      provider: importProvider.value,
      host: isGeneric ? importHost.value.trim() : undefined,
      image_prefix:
        (isGeneric ? importPrefix.value.trim() : importNamespace.value) || undefined,
      ...importCreds(),
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
          Copies <strong>every repository, tag and architecture</strong> from an upstream registry
          into a target org. Runs in the background; blobs already present are skipped.
        </p>
        <div>
          <label class="mb-1.5 block text-sm font-medium">Provider</label>
          <select
            v-model="importProvider"
            data-testid="import-provider"
            class="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            @change="onProviderChange"
          >
            <option value="generic">Generic (OCI catalog)</option>
            <option value="digitalocean">DigitalOcean</option>
            <option value="github">GitHub (GHCR)</option>
          </select>
        </div>
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

        <!-- generic: free-text host + optional prefix filter -->
        <template v-if="importProvider === 'generic'">
          <div>
            <label class="mb-1.5 block text-sm font-medium">Registry host</label>
            <UiInput
              v-model="importHost"
              placeholder="registry.example.com"
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
              placeholder="team / namespace"
              data-testid="import-prefix"
            />
            <p class="mt-1.5 text-xs text-muted-foreground">
              Only import repositories under this namespace (matched on a
              <code>/</code> boundary). Leave empty to import the whole catalog.
            </p>
          </div>
        </template>

        <!-- credentials -->
        <div v-if="importProvider === 'digitalocean'">
          <label class="mb-1.5 block text-sm font-medium">API token</label>
          <UiInput
            v-model="importPass"
            type="password"
            placeholder="dop_v1_…"
            data-testid="import-pass"
          />
          <p class="mt-1.5 text-xs text-muted-foreground">
            Your DigitalOcean API token (used as both username and password).
          </p>
        </div>
        <div v-else class="grid grid-cols-2 gap-3">
          <div>
            <label class="mb-1.5 block text-sm font-medium">Username</label>
            <UiInput
              v-model="importUser"
              :placeholder="importProvider === 'github' ? 'github-user' : 'token / user'"
              data-testid="import-user"
            />
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
        <p v-if="importProvider === 'github'" class="text-xs text-muted-foreground">
          GitHub username + a personal access token with the <code>read:packages</code> scope.
        </p>

        <!-- API-backed providers: pick a namespace loaded from their API -->
        <div v-if="importProvider !== 'generic'">
          <label class="mb-1.5 block text-sm font-medium">
            {{ importProvider === "digitalocean" ? "Registry" : "Owner" }}
          </label>
          <div class="flex gap-2">
            <select
              v-model="importNamespace"
              data-testid="import-namespace"
              :disabled="!importNamespaces.length"
              class="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
            >
              <option value="" disabled>
                {{ importNamespaces.length ? "Select…" : "Load to choose…" }}
              </option>
              <option v-for="n in importNamespaces" :key="n.name" :value="n.name">
                {{ n.name }}<template v-if="n.repo_count != null"> ({{ n.repo_count }} repos)</template>
              </option>
            </select>
            <UiButton
              type="button"
              variant="secondary"
              :disabled="discovering"
              data-testid="import-discover"
              @click="discoverNamespaces"
            >
              {{ discovering ? "Loading…" : "Load" }}
            </UiButton>
          </div>
          <p v-if="discoverError" class="mt-1.5 text-xs text-destructive">{{ discoverError }}</p>
          <p v-else class="mt-1.5 text-xs text-muted-foreground">
            Enter your token, then <strong>Load</strong> to list your
            {{ importProvider === "digitalocean" ? "registries" : "owners" }}.
          </p>
        </div>

        <p v-if="importError" class="text-sm text-destructive">{{ importError }}</p>
        <div class="flex justify-end gap-2">
          <UiButton type="button" variant="ghost" @click="closeImport">Cancel</UiButton>
          <UiButton
            type="submit"
            :disabled="importing || !canStartImport"
            data-testid="import-submit"
          >
            {{ importing ? "Starting…" : "Start import" }}
          </UiButton>
        </div>
      </form>
    </UiModal>
  </div>
</template>
