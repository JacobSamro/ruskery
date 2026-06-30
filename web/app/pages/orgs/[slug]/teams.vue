<script setup lang="ts">
const route = useRoute();
const api = useApi();
const me = useMe();
const slug = computed(() => route.params.slug as string);

const teams = ref<Team[]>([]);
const selected = ref<Team | null>(null);
const teamMembers = ref<Member[]>([]);
const teamPerms = ref<TeamPerm[]>([]);
const error = ref("");

const showCreate = ref(false);
const newTeam = reactive({ name: "", slug: "" });
const addMemberLogin = ref("");
const permRepo = ref("");
const permLevel = ref("pull");

const canAdmin = computed(() => {
  const role = me.value?.orgs.find((o) => o.slug === slug.value)?.role;
  return role === "owner" || role === "admin";
});

watch(() => newTeam.name, (v) => {
  newTeam.slug = v.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
});

async function loadTeams() {
  teams.value = (await api.get<{ teams: Team[] }>(`/api/v1/orgs/${slug.value}/teams`)).teams;
  if (!selected.value && teams.value.length) select(teams.value[0]);
}
onMounted(loadTeams);
watch(slug, () => {
  selected.value = null;
  loadTeams();
});

async function select(t: Team) {
  selected.value = t;
  const [m, p] = await Promise.all([
    api.get<{ members: Member[] }>(`/api/v1/orgs/${slug.value}/teams/${t.slug}/members`),
    api.get<{ permissions: TeamPerm[] }>(`/api/v1/orgs/${slug.value}/teams/${t.slug}/perms`),
  ]);
  teamMembers.value = m.members;
  teamPerms.value = p.permissions;
}

async function create() {
  error.value = "";
  try {
    await api.post(`/api/v1/orgs/${slug.value}/teams`, { name: newTeam.name, slug: newTeam.slug });
    showCreate.value = false;
    newTeam.name = "";
    await loadTeams();
  } catch (e) {
    error.value = apiErrorMessage(e);
  }
}

async function addMember() {
  if (!selected.value) return;
  try {
    await api.post(`/api/v1/orgs/${slug.value}/teams/${selected.value.slug}/members`, { login: addMemberLogin.value });
    addMemberLogin.value = "";
    await select(selected.value);
  } catch (e) {
    error.value = apiErrorMessage(e);
  }
}

async function removeMember(m: Member) {
  if (!selected.value) return;
  await api.del(`/api/v1/orgs/${slug.value}/teams/${selected.value.slug}/members/${m.user_id}`);
  await select(selected.value);
}

async function setPerm() {
  if (!selected.value) return;
  try {
    await api.post(`/api/v1/orgs/${slug.value}/teams/${selected.value.slug}/perms`, {
      repo: permRepo.value,
      permission: permLevel.value,
    });
    permRepo.value = "";
    await select(selected.value);
  } catch (e) {
    error.value = apiErrorMessage(e);
  }
}
</script>

<template>
  <div>
    <div class="mb-6 flex items-end justify-between">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">Teams</h1>
        <p class="text-sm text-[var(--color-muted)]">Group members and grant per-repository access.</p>
      </div>
      <UiButton v-if="canAdmin" size="sm" @click="showCreate = true">
        <UiIcon name="plus" :size="14" /> New team
      </UiButton>
    </div>

    <div class="grid grid-cols-3 gap-6">
      <UiCard class="col-span-1">
        <div v-if="!teams.length" class="py-6 text-center text-sm text-[var(--color-muted)]">No teams yet.</div>
        <ul class="flex flex-col gap-1">
          <li v-for="t in teams" :key="t.id">
            <button
              class="flex w-full items-center gap-2 rounded-[var(--radius)] px-3 py-2 text-left text-sm transition-colors"
              :class="selected?.id === t.id ? 'bg-[var(--color-bg)] font-medium' : 'text-[var(--color-muted)] hover:bg-[var(--color-bg)]'"
              @click="select(t)"
            >
              <UiIcon name="team" :size="16" /> {{ t.name }}
            </button>
          </li>
        </ul>
      </UiCard>

      <div class="col-span-2 flex flex-col gap-6">
        <UiCard v-if="selected" :title="`${selected.name} · members`">
          <template #header>
            <div v-if="canAdmin" class="flex gap-2">
              <UiInput v-model="addMemberLogin" placeholder="username" class="h-8 w-40" />
              <UiButton size="sm" @click="addMember">Add</UiButton>
            </div>
          </template>
          <div v-if="!teamMembers.length" class="py-4 text-sm text-[var(--color-muted)]">No members.</div>
          <ul v-else class="flex flex-col divide-y divide-[var(--color-border)]">
            <li v-for="m in teamMembers" :key="m.user_id" class="flex items-center justify-between py-2 text-sm">
              <span>{{ m.username }} <span class="text-[var(--color-muted)]">· {{ m.role }}</span></span>
              <UiButton v-if="canAdmin" variant="ghost" size="sm" @click="removeMember(m)">
                <UiIcon name="trash" :size="14" />
              </UiButton>
            </li>
          </ul>
        </UiCard>

        <UiCard v-if="selected" :title="`${selected.name} · repository access`">
          <template #header>
            <div v-if="canAdmin" class="flex gap-2">
              <UiInput v-model="permRepo" placeholder="repo name" class="h-8 w-32" />
              <select v-model="permLevel" class="h-8 rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-bg)] px-2 text-xs">
                <option value="pull">pull</option>
                <option value="push">push</option>
                <option value="admin">admin</option>
              </select>
              <UiButton size="sm" @click="setPerm">Grant</UiButton>
            </div>
          </template>
          <div v-if="!teamPerms.length" class="py-4 text-sm text-[var(--color-muted)]">No grants.</div>
          <ul v-else class="flex flex-col divide-y divide-[var(--color-border)]">
            <li v-for="p in teamPerms" :key="p.repo" class="flex items-center justify-between py-2 text-sm">
              <span class="font-mono">{{ p.repo }}</span>
              <UiBadge variant="outline">{{ p.permission }}</UiBadge>
            </li>
          </ul>
        </UiCard>

        <p v-if="error" class="text-sm text-red-400">{{ error }}</p>
      </div>
    </div>

    <UiModal :open="showCreate" title="New team" @close="showCreate = false">
      <form class="flex flex-col gap-4" @submit.prevent="create">
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Name</label>
          <UiInput v-model="newTeam.name" placeholder="Backend" required />
        </div>
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Slug</label>
          <UiInput v-model="newTeam.slug" placeholder="backend" required />
        </div>
        <div class="flex justify-end gap-2">
          <UiButton variant="outline" type="button" @click="showCreate = false">Cancel</UiButton>
          <UiButton type="submit">Create</UiButton>
        </div>
      </form>
    </UiModal>
  </div>
</template>
