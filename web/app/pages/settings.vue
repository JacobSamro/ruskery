<script setup lang="ts">
// Instance-level settings (super-admin only): storage, TLS/domains, Google
// sign-in, and instance-wide user accounts. These are global to the whole
// ruskery instance — not scoped to any organization.
const api = useApi();
const me = useMe();
const router = useRouter();

// Instance-admin user management.
const users = ref<User[]>([]);
const showAddUser = ref(false);
const nu = reactive({ username: "", email: "", password: "", is_admin: false });
const error = ref("");

// Storage (Tigris) settings.
const storage = reactive({
  endpoint: "",
  bucket: "",
  region: "auto",
  access_key_id: "",
  secret_access_key: "",
  secret_set: false,
  cdn_url: "",
  force_path_style: false,
  presign_ttl_secs: 900,
});
const storageError = ref("");
const storageSaved = ref(false);

async function loadStorage() {
  const s = await api.get<typeof storage>("/api/v1/settings/storage");
  Object.assign(storage, s, { secret_access_key: "" });
}

async function saveStorage() {
  storageError.value = "";
  storageSaved.value = false;
  try {
    const body: Record<string, unknown> = {
      endpoint: storage.endpoint,
      bucket: storage.bucket,
      region: storage.region,
      access_key_id: storage.access_key_id,
      cdn_url: storage.cdn_url,
      force_path_style: storage.force_path_style,
      presign_ttl_secs: Number(storage.presign_ttl_secs),
    };
    if (storage.secret_access_key) body.secret_access_key = storage.secret_access_key;
    await api.put("/api/v1/settings/storage", body);
    storage.secret_access_key = "";
    storageSaved.value = true;
    await loadStorage();
    setTimeout(() => (storageSaved.value = false), 2000);
  } catch (e) {
    storageError.value = apiErrorMessage(e);
  }
}

// Google sign-in (OAuth).
const oauth = reactive({
  enabled: false,
  client_id: "",
  client_secret: "",
  secret_set: false,
  allowed_domain: "",
  redirect_uri: "",
});
const oauthError = ref("");
const oauthSaved = ref(false);
const redirectCopied = ref(false);

async function loadOauth() {
  const o = await api.get<typeof oauth>("/api/v1/settings/oauth");
  Object.assign(oauth, o, { client_secret: "" });
}

async function saveOauth() {
  oauthError.value = "";
  oauthSaved.value = false;
  try {
    const body: Record<string, unknown> = {
      enabled: oauth.enabled,
      client_id: oauth.client_id,
      allowed_domain: oauth.allowed_domain,
    };
    if (oauth.client_secret) body.client_secret = oauth.client_secret;
    await api.put("/api/v1/settings/oauth", body);
    oauth.client_secret = "";
    oauthSaved.value = true;
    await loadOauth();
    setTimeout(() => (oauthSaved.value = false), 2000);
  } catch (e) {
    oauthError.value = apiErrorMessage(e);
  }
}

function copyRedirect() {
  navigator.clipboard?.writeText(oauth.redirect_uri);
  redirectCopied.value = true;
  setTimeout(() => (redirectCopied.value = false), 1500);
}

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
const contactEmail = ref("");
const contactSaved = ref(false);
const contactError = ref("");

async function loadDomains() {
  const d = await api.get<{
    domains: DomainRow[];
    tls_enabled: boolean;
    contact_email: string;
  }>("/api/v1/domains");
  domains.value = d.domains;
  tlsEnabled.value = d.tls_enabled;
  contactEmail.value = d.contact_email ?? "";
}

async function saveContactEmail() {
  contactError.value = "";
  contactSaved.value = false;
  try {
    await api.put("/api/v1/settings/tls", { contact_email: contactEmail.value.trim() });
    contactSaved.value = true;
  } catch (e) {
    contactError.value = apiErrorMessage(e);
  }
}

async function addDomain() {
  domainError.value = "";
  if (!contactEmail.value.trim()) {
    domainError.value = "Add a Let's Encrypt contact email above before adding a domain.";
    return;
  }
  try {
    // Persist the email first so the certificate request has a contact.
    await saveContactEmail();
    if (contactError.value) {
      domainError.value = contactError.value;
      return;
    }
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
  users.value = (await api.get<{ users: User[] }>("/api/v1/users")).users;
}

async function addUser() {
  error.value = "";
  try {
    await api.post("/api/v1/users", { ...nu });
    showAddUser.value = false;
    nu.username = nu.email = nu.password = "";
    nu.is_admin = false;
    await loadUsers();
  } catch (e) {
    error.value = apiErrorMessage(e);
  }
}

onMounted(() => {
  if (!me.value?.user.is_admin) {
    router.replace("/");
    return;
  }
  loadUsers();
  loadDomains();
  loadStorage();
  loadOauth();
});
</script>

<template>
  <div>
    <h1 class="mb-1 text-2xl font-semibold tracking-tight">Instance settings</h1>
    <p class="mb-6 text-sm text-[var(--color-muted)]">
      Global configuration for this ruskery instance — applies to every organization.
    </p>

    <div class="flex flex-col gap-6">
      <UiCard
        title="Storage (Tigris)"
        description="S3-compatible backend for image layers. Use the CDN URL to serve pulls from a Tigris custom domain."
      >
        <form class="flex flex-col gap-4" @submit.prevent="saveStorage">
          <div class="grid grid-cols-2 gap-3">
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">S3 endpoint</label>
              <UiInput v-model="storage.endpoint" placeholder="https://t3.storage.dev" />
            </div>
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">Bucket</label>
              <UiInput v-model="storage.bucket" placeholder="my-registry-bucket" />
            </div>
          </div>

          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">CDN URL <span class="text-[var(--color-muted)]">(custom domain for pulls — optional)</span></label>
            <UiInput v-model="storage.cdn_url" placeholder="https://cdn.yourcompany.com" />
            <p class="text-xs text-[var(--color-muted)]">
              When set, pull redirects are signed for and served from this host instead of the S3 endpoint.
            </p>
          </div>

          <div class="grid grid-cols-2 gap-3">
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">Access key ID</label>
              <UiInput v-model="storage.access_key_id" />
            </div>
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">Secret access key</label>
              <UiInput
                v-model="storage.secret_access_key"
                type="password"
                :placeholder="storage.secret_set ? '•••••••• (unchanged)' : 'set a secret'"
              />
            </div>
          </div>

          <div class="grid grid-cols-3 items-end gap-3">
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">Region</label>
              <UiInput v-model="storage.region" placeholder="auto" />
            </div>
            <div class="flex flex-col gap-1.5">
              <label class="text-sm font-medium">Presign TTL (s)</label>
              <UiInput v-model="storage.presign_ttl_secs" type="number" />
            </div>
            <label class="flex items-center gap-2 pb-2 text-sm">
              <input v-model="storage.force_path_style" type="checkbox" /> Path-style
            </label>
          </div>

          <div class="flex items-center justify-between">
            <p v-if="storageError" class="text-sm text-red-400">{{ storageError }}</p>
            <p v-else-if="storageSaved" class="text-sm text-[var(--color-primary)]">Saved — applied live.</p>
            <span v-else></span>
            <UiButton type="submit">Save storage</UiButton>
          </div>
        </form>
      </UiCard>

      <UiCard
        title="Sign in with Google"
        description="Let users authenticate with Google. Create an OAuth client in Google Cloud and paste its credentials here."
      >
        <form class="flex flex-col gap-4" @submit.prevent="saveOauth">
          <label class="flex items-center gap-2 text-sm">
            <input v-model="oauth.enabled" type="checkbox" /> Enable Google sign-in
          </label>

          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Authorized redirect URI</label>
            <div class="flex items-center gap-2 rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] px-3 py-2 font-mono text-xs">
              <span class="flex-1 break-all">{{ oauth.redirect_uri }}</span>
              <UiButton variant="ghost" size="sm" type="button" @click="copyRedirect">
                <UiIcon :name="redirectCopied ? 'check' : 'copy'" :size="14" />
              </UiButton>
            </div>
            <p class="text-xs text-[var(--color-muted)]">
              Add this exact URL under <span class="text-[var(--color-fg)]">APIs &amp; Services → Credentials →
              your OAuth client → Authorized redirect URIs</span> in the Google Cloud console.
              (Set <code class="text-[var(--color-fg)]">server.public_url</code> so this stays stable.)
            </p>
          </div>

          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Client ID</label>
            <UiInput v-model="oauth.client_id" placeholder="xxxxx.apps.googleusercontent.com" />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Client secret</label>
            <UiInput
              v-model="oauth.client_secret"
              type="password"
              :placeholder="oauth.secret_set ? '•••••••• (unchanged)' : 'paste client secret'"
            />
          </div>
          <div class="flex flex-col gap-1.5">
            <label class="text-sm font-medium">Allowed email domain <span class="text-[var(--color-muted)]">(optional)</span></label>
            <UiInput v-model="oauth.allowed_domain" placeholder="yourcompany.com" />
            <p class="text-xs text-[var(--color-muted)]">
              If set, only verified emails in this domain may sign in — and they're auto-created on first
              login. Leave blank to allow Google sign-in only for users that already have an account.
            </p>
          </div>

          <div class="flex items-center justify-between">
            <p v-if="oauthError" class="text-sm text-red-400">{{ oauthError }}</p>
            <p v-else-if="oauthSaved" class="text-sm text-[var(--color-primary)]">Saved.</p>
            <span v-else></span>
            <UiButton type="submit">Save Google sign-in</UiButton>
          </div>
        </form>
      </UiCard>

      <UiCard
        title="Domains & TLS"
        description="Connect a domain; certificates are issued automatically via Let's Encrypt."
      >
        <div
          v-if="!tlsEnabled"
          class="mb-4 flex items-start gap-2 rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] p-3 text-sm text-[var(--color-muted)]"
        >
          <UiIcon name="shield" :size="16" class="mt-0.5" />
          <span>Automatic TLS is disabled in this instance's config. Set <code class="text-[var(--color-fg)]">tls.enabled = true</code> (it's on by default) and restart to provision certificates.</span>
        </div>

        <p class="mb-3 text-sm text-[var(--color-muted)]">
          Point an <span class="font-mono text-[var(--color-fg)]">A</span> record for your domain at this
          server's IP, then add it below. ruskery requests a certificate once DNS resolves here.
        </p>

        <div class="mb-4">
          <label class="text-sm font-medium">
            Let's Encrypt contact email
            <span class="text-[var(--color-muted)]">(required before adding a domain)</span>
          </label>
          <div class="mt-1 flex gap-2">
            <UiInput
              v-model="contactEmail"
              type="email"
              placeholder="admin@yourcompany.com"
              class="flex-1"
              @input="contactSaved = false"
            />
            <UiButton variant="outline" @click="saveContactEmail">Save</UiButton>
          </div>
          <p v-if="contactError" class="mt-1 text-sm text-red-400">{{ contactError }}</p>
          <p v-else-if="contactSaved" class="mt-1 text-sm text-[var(--color-muted)]">
            Saved — used when registering with Let's Encrypt.
          </p>
        </div>

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

      <UiCard title="Users" description="Instance-wide accounts (admin only).">
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
