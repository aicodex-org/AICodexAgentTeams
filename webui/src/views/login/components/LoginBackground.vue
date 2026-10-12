<script setup lang="ts">
import LaniakeaScene from '@/components/cosmic/LaniakeaScene.vue'
</script>

<template>
  <div class="login-universe" aria-hidden="true" inert>
    <LaniakeaScene :playing="false" decorative />
  </div>
</template>

<style scoped>
.login-universe {
  position: absolute;
  inset: 0;
  overflow: hidden;
  pointer-events: none;
  opacity: 0.72;
}
</style>
