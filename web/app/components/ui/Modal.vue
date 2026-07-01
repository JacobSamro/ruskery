<script setup lang="ts">
defineProps<{ open: boolean; title?: string }>();
const emit = defineEmits<{ close: [] }>();
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      @click.self="emit('close')"
    >
      <div
        class="w-full max-w-md rounded-xl border border-border bg-card text-card-foreground p-6 shadow-xl"
      >
        <h2 v-if="title" class="mb-4 text-lg font-semibold">{{ title }}</h2>
        <slot />
      </div>
    </div>
  </Teleport>
</template>
