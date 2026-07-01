<script setup lang="ts">
// `UiCard` — the project's card API (title/description props + default & #header
// slots), composed from the official shadcn-vue Card sub-components. The #header
// slot maps to CardAction (right-aligned via CardHeader's grid).
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  CardAction,
} from "@/components/ui/card";

defineProps<{ title?: string; description?: string }>();
</script>

<template>
  <Card>
    <CardHeader v-if="title || $slots.header">
      <CardTitle v-if="title">{{ title }}</CardTitle>
      <CardDescription v-if="description">{{ description }}</CardDescription>
      <CardAction v-if="$slots.header">
        <slot name="header" />
      </CardAction>
    </CardHeader>
    <CardContent>
      <slot />
    </CardContent>
  </Card>
</template>
