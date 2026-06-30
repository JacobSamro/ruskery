<script setup lang="ts">
const route = useRoute();
const api = useApi();
const me = useMe();
const slug = computed(() => route.params.slug as string);

const host = computed(() => window.location.host);

// Instance-admin user management.
const users = ref<User[]>([]);
const showAddUser = ref(false);
const nu = reactive({ username: "", email: "", password: "", is_admin: false });
const error = ref("");

// Custom domains / TLS.
interface DomainRow {
  domain: string;
  status: string;
  is_primary: boolean;
  detail: string | null;
}
const domains = ref<DomainRow[]>([]);
const tlsEnabled = ref(false);
const newDomain = ref("");
const domainError = ref("");

async function loadDomains() {
  if (!me.value?.user.is_admin) return;
  const d = await api.get<{ domains: DomainRow[]; tls_enabled: boolean }>("/api/v1/domains");
  domains.value = d.domains;
  tlsEnabled.value = d.tls_enabled;
}

async function addDomain() {
  domainError.value = "";
  try {
    await api.post("/api/v1/domains", { domain: newDomain.value });
    newDomain.value = "";
    await loadDomains();
  } catch (e) {
    domainError.value = apiErrorMessage(e);
  }
}
async function removeDomain(d: string) {
  if (!confirm(`Remove ${d}?`)) return;
  await api.del(`/api/v1/domains/${d}`);
  await loadDomains();
}
async function makePrimary(d: string) {
  await api.post(`/api/v1/domains/${d}/primary`);
  await loadDomains();
}

async function loadUsers() {
  if (!me.value?.user.is_admin) return;
  users.value = (await api.get<{ users: User[] }>("/api/v1/users")).users;
}
onMounted(() => {
  loadUsers();
  loadDomains();
});

async function addUser() {
  error.value = "";
  try {
    await api.post("/api/v1/users", { ...nu });
    showAddUser.value = false;
    nu.username = nu.email = nu.password = "";
    await loadUsers();
  } catch (e) {
    error.value = apiErrorMessage(e);
  }
}
</script>

<template>
  <div>
    <h1 class="mb-6 text-2xl font-semibold tracking-tight">Settings</h1>

    <div class="flex flex-col gap-6">
      <UiCard title="Organization" description="Namespace for your repositories.">
        <dl class="grid grid-cols-2 gap-4 text-sm">
          <div>
            <dt class="text-[var(--color-muted)]">Slug</dt>
            <dd class="font-mono">{{ slug }}</dd>
          </div>
          <div>
            <dt class="text-[var(--color-muted)]">Registry host</dt>
            <dd class="font-mono">{{ host }}</dd>
          </div>
        </dl>
      </UiCard>

      <UiCard title="Quick start" description="Authenticate and push your first image.">
        <pre class="overflow-x-auto rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] p-4 text-xs leading-relaxed"><code>docker login {{ host }} -u {{ me?.user.username }} -p &lt;access-token&gt;
docker tag my-image {{ host }}/{{ slug }}/my-image:latest
docker push {{ host }}/{{ slug }}/my-image:latest</code></pre>
      </UiCard>

      <UiCard
        v-if="me?.user.is_admin"
        title="Domains & TLS"
        description="Connect a domain; certificates are issued automatically via Let's Encrypt."
      >
        <div
          v-if="!tlsEnabled"
          class="mb-4 flex items-start gap-2 rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] p-3 text-sm text-[var(--color-muted)]"
        >
          <UiIcon name="shield" :size="16" class="mt-0.5" />
          <span>Automatic TLS is off. Set <code class="text-[var(--color-fg)]">tls.enabled = true</code> in the config and restart to provision certificates.</span>
        </div>

        <p class="mb-3 text-sm text-[var(--color-muted)]">
          Point an <span class="font-mono text-[var(--color-fg)]">A</span> record for your domain at this
          server's IP, then add it below. ruskery requests a certificate once DNS resolves here.
        </p>

        <div class="mb-4 flex gap-2">
          <UiInput v-model="newDomain" placeholder="registry.yourcompany.com" class="flex-1" />
          <UiButton @click="addDomain"><UiIcon name="globe" :size="14" /> Add domain</UiButton>
        </div>
        <p v-if="domainError" class="mb-3 text-sm text-red-400">{{ domainError }}</p>

        <div v-if="!domains.length" class="py-4 text-center text-sm text-[var(--color-muted)]">
          No custom domains yet.
        </div>
        <ul v-else class="flex flex-col divide-y divide-[var(--color-border)]">
          <li v-for="d in domains" :key="d.domain" class="flex items-center justify-between py-3 text-sm">
            <div class="flex items-center gap-2">
              <span class="font-mono">{{ d.domain }}</span>
              <UiBadge v-if="d.is_primary" variant="primary">primary</UiBadge>
              <UiBadge :variant="d.status === 'active' ? 'primary' : 'outline'">{{ d.status }}</UiBadge>
            </div>
            <div class="flex gap-1">
              <UiButton v-if="!d.is_primary" variant="ghost" size="sm" @click="makePrimary(d.domain)">
                Make primary
              </UiButton>
              <UiButton variant="ghost" size="sm" @click="removeDomain(d.domain)">
                <UiIcon name="trash" :size="14" />
              </UiButton>
            </div>
          </li>
        </ul>
      </UiCard>

      <UiCard v-if="me?.user.is_admin" title="Users" description="Instance-wide accounts (admin only).">
        <template #header>
          <UiButton size="sm" @click="showAddUser = true"><UiIcon name="plus" :size="14" /> New user</UiButton>
        </template>
        <table class="w-full text-sm">
          <tbody>
            <tr v-for="u in users" :key="u.id" class="border-b border-[var(--color-border)] last:border-0">
              <td class="py-2 font-medium">{{ u.username }}</td>
              <td class="py-2 text-[var(--color-muted)]">{{ u.email }}</td>
              <td class="py-2 text-right">
                <UiBadge v-if="u.is_admin" variant="primary">admin</UiBadge>
              </td>
            </tr>
          </tbody>
        </table>
      </UiCard>
    </div>

    <UiModal :open="showAddUser" title="New user" @close="showAddUser = false">
      <form class="flex flex-col gap-4" @submit.prevent="addUser">
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Username</label>
          <UiInput v-model="nu.username" required />
        </div>
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Email</label>
          <UiInput v-model="nu.email" type="email" required />
        </div>
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Password</label>
          <UiInput v-model="nu.password" type="password" placeholder="at least 8 characters" required />
        </div>
        <label class="flex items-center gap-2 text-sm">
          <input v-model="nu.is_admin" type="checkbox" /> Instance admin
        </label>
        <p v-if="error" class="text-sm text-red-400">{{ error }}</p>
        <div class="flex justify-end gap-2">
          <UiButton variant="outline" type="button" @click="showAddUser = false">Cancel</UiButton>
          <UiButton type="submit">Create</UiButton>
        </div>
      </form>
    </UiModal>
  </div>
</template>
