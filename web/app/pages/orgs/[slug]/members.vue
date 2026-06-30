<script setup lang="ts">
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const route = useRoute();
const api = useApi();
const me = useMe();
const { confirm } = useConfirm();
const slug = computed(() => route.params.slug as string);

const members = ref<Member[]>([]);
const loading = ref(true);
const showAdd = ref(false);
const addLogin = ref("");
const addRole = ref("member");
const error = ref("");

const canAdmin = computed(() => {
  const role = me.value?.orgs.find((o) => o.slug === slug.value)?.role;
  return role === "owner" || role === "admin";
});

async function load() {
  loading.value = true;
  try {
    members.value = (await api.get<{ members: Member[] }>(`/api/v1/orgs/${slug.value}/members`)).members;
  } finally {
    loading.value = false;
  }
}
onMounted(load);
watch(slug, load);

async function add() {
  error.value = "";
  try {
    await api.post(`/api/v1/orgs/${slug.value}/members`, { login: addLogin.value, role: addRole.value });
    showAdd.value = false;
    addLogin.value = "";
    await load();
  } catch (e) {
    error.value = apiErrorMessage(e);
  }
}

async function setRole(m: Member, role: unknown) {
  await api.post(`/api/v1/orgs/${slug.value}/members/${m.user_id}`, { role: String(role) });
  await load();
}

async function remove(m: Member) {
  if (
    !(await confirm({
      title: "Remove member",
      message: `Remove ${m.username} from this organization?`,
      confirmText: "Remove",
      destructive: true,
    }))
  )
    return;
  await api.del(`/api/v1/orgs/${slug.value}/members/${m.user_id}`);
  await load();
}
</script>

<template>
  <div>
    <div class="mb-6 flex items-end justify-between">
      <div>
        <h1 class="text-2xl font-semibold tracking-tight">Members</h1>
        <p class="text-sm text-[var(--color-muted)]">People in this organization and their roles.</p>
      </div>
      <UiButton v-if="canAdmin" size="sm" @click="showAdd = true">
        <UiIcon name="plus" :size="14" /> Add member
      </UiButton>
    </div>

    <UiCard>
      <div v-if="loading" class="py-8 text-center text-sm text-[var(--color-muted)]">Loading…</div>
      <table v-else class="w-full text-sm">
        <thead class="text-left text-xs uppercase tracking-wide text-[var(--color-muted)]">
          <tr class="border-b border-[var(--color-border)]">
            <th class="px-3 py-2 font-medium">User</th>
            <th class="px-3 py-2 font-medium">Email</th>
            <th class="px-3 py-2 font-medium">Role</th>
            <th class="px-3 py-2"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="m in members" :key="m.user_id" class="border-b border-[var(--color-border)] last:border-0">
            <td class="px-3 py-3 font-medium">{{ m.username }}</td>
            <td class="px-3 py-3 text-[var(--color-muted)]">{{ m.email }}</td>
            <td class="px-3 py-3">
              <Select
                v-if="canAdmin"
                :model-value="m.role"
                @update:model-value="(v) => setRole(m, v)"
              >
                <SelectTrigger class="h-8 w-32"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="member">member</SelectItem>
                  <SelectItem value="admin">admin</SelectItem>
                  <SelectItem value="owner">owner</SelectItem>
                </SelectContent>
              </Select>
              <UiBadge v-else>{{ m.role }}</UiBadge>
            </td>
            <td class="px-3 py-3 text-right">
              <UiButton v-if="canAdmin" variant="ghost" size="sm" @click="remove(m)">
                <UiIcon name="trash" :size="14" />
              </UiButton>
            </td>
          </tr>
        </tbody>
      </table>
    </UiCard>

    <UiModal :open="showAdd" title="Add member" @close="showAdd = false">
      <form class="flex flex-col gap-4" @submit.prevent="add">
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Username or email</label>
          <UiInput v-model="addLogin" placeholder="existing user" required />
        </div>
        <div class="flex flex-col gap-1.5">
          <label class="text-sm font-medium">Role</label>
          <Select v-model="addRole">
            <SelectTrigger class="w-full"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="member">member</SelectItem>
              <SelectItem value="admin">admin</SelectItem>
              <SelectItem value="owner">owner</SelectItem>
            </SelectContent>
          </Select>
        </div>
        <p v-if="error" class="text-sm text-red-400">{{ error }}</p>
        <div class="flex justify-end gap-2">
          <UiButton variant="outline" type="button" @click="showAdd = false">Cancel</UiButton>
          <UiButton type="submit">Add</UiButton>
        </div>
      </form>
    </UiModal>
  </div>
</template>
