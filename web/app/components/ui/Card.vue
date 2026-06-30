<script setup lang="ts">
defineProps<{ title?: string; description?: string }>();
</script>

<template>
  <div class="rounded-[var(--radius)] border border-[var(--color-border)] bg-[var(--color-surface)] shadow-sm">
    <div v-if="title || $slots.header" class="flex items-start justify-between gap-4 p-5 pb-0">
      <div v-if="title">
        <h3 class="font-semibold leading-none tracking-tight">{{ title }}</h3>
        <p v-if="description" class="mt-1.5 text-sm text-[var(--color-muted)]">{{ description }}</p>
      </div>
      <slot name="header" />
    </div>
    <div class="p-5">
      <slot />
    </div>
  </div>
</template>
