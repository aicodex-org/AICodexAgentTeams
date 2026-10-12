<script setup lang="ts">
import { ArrowRight, ExternalLink, Pause, Play, RotateCcw } from '@lucide/vue'
import { usePreferredReducedMotion } from '@vueuse/core'
import { computed, shallowRef, useTemplateRef } from 'vue'
import { RouterLink } from 'vue-router'
import AppBrandMark from '@/components/AppBrandMark.vue'
import LaniakeaScene from '@/components/cosmic/LaniakeaScene.vue'
import LoginThemeToggle from '@/views/login/components/LoginThemeToggle.vue'
import '@/components/cosmic/surface.css'

const scene = useTemplateRef('scene')
const playing = shallowRef(true)
const generation = shallowRef(0)
const lineCount = shallowRef(0)
const failed = shallowRef(false)
const reducedMotion = usePreferredReducedMotion()
const animationEnabled = computed(() => playing.value && reducedMotion.value !== 'reduce')
function retry() {
  lineCount.value = 0
  failed.value = false
  generation.value += 1
}
</script>

<template>
  <main class="home-page cosmic-surface">
    <header class="home-header">
      <RouterLink to="/" class="brand" aria-label="AICodexAgentTeams 主页">
        <AppBrandMark class="brand-mark" />
        <span>AICodexAgentTeams</span>
      </RouterLink>
      <nav aria-label="主导航">
        <LoginThemeToggle />
        <RouterLink :to="{ name: 'login', query: { redirect: '/dashboard' } }" class="console-link">
          进入控制台 <ArrowRight :size="16" />
        </RouterLink>
      </nav>
    </header>

    <section class="universe" aria-labelledby="home-title">
      <div class="title-block">
        <p class="eyebrow">
          <span /> OUR COSMIC HOME
        </p>
        <h1 id="home-title">
          Laniakea<span>拉尼亚凯亚超星系团</span>
        </h1>
        <p class="intro">
          沿着引力的流向，看见我们在宇宙中的位置
        </p>
      </div>

      <div class="scene-area">
        <LaniakeaScene ref="scene" :key="generation" :playing="playing" @ready="lineCount = $event" @retry="retry" @failed="failed = true" />
      </div>

      <div class="scene-caption" aria-live="polite">
        <span class="status-dot" />
        {{ failed ? '星图暂不可用' : lineCount ? `${lineCount.toLocaleString()} 条速度场流线` : '正在加载宇宙流线' }}
        <span class="caption-divider">/</span> COSMICFLOWS–4
      </div>

      <div class="scene-toolbar" aria-label="星图控制">
        <span class="gesture-hint">拖动旋转 · 滚轮缩放</span>
        <button
          type="button"
          :aria-label="animationEnabled ? '暂停动画' : '播放动画'"
          :aria-pressed="animationEnabled"
          :disabled="reducedMotion === 'reduce'"
          :title="reducedMotion === 'reduce' ? '已遵循系统减少动态效果设置' : animationEnabled ? '暂停动画' : '播放动画'"
          @click="playing = !playing"
        >
          <Pause v-if="animationEnabled" :size="16" /><Play v-else :size="16" />
        </button>
        <button type="button" aria-label="重置视角" title="重置视角" @click="scene?.reset()">
          <RotateCcw :size="16" />
        </button>
      </div>
    </section>

    <footer class="home-footer">
      <div class="legend">
        <span class="flow-swatch" /> 引力流线 <span class="local-dot" /> 银河系 <span class="attractor-dot" /> 巨引源
      </div>
      <details class="sources">
        <summary>关于这幅星图 <ExternalLink :size="12" /></summary>
        <div class="source-panel">
          <strong>真实数据，流线可视化</strong>
          <p>基于 CosmicFlows-4 非分组速度场与拉尼亚凯亚盆地采样生成，线条表示重建流向，光点用于演示方向，不表示星系位置或实际运行速度</p>
          <p>坐标为超星系坐标 SGX / SGY / SGZ，单位 Mpc/h，银河系在此尺度下近似位于本星系群原点</p>
          <p>巨引源以 CF4 非分组重建的拉尼亚凯亚吸引中心 [−62, −8, 39] 标示，是模型估计位置，不代表单个天体</p>
          <a href="https://projets.ip2i.in2p3.fr/cosmicflows/" target="_blank" rel="noreferrer">CosmicFlows 官方数据 ↗</a>
          <a href="https://arxiv.org/abs/2211.16390" target="_blank" rel="noreferrer">Courtois 等 · 2023 · 速度场 ↗</a>
          <a href="https://arxiv.org/abs/2305.02339" target="_blank" rel="noreferrer">Dupuy & Courtois · 2023 · 引力盆地 ↗</a>
        </div>
      </details>
      <span class="footer-coordinate">SGX / SGY / SGZ</span>
    </footer>
  </main>
</template>

<style scoped>
.home-page {
  display: flex;
  min-height: 100dvh;
  flex-direction: column;
}
.home-header {
  z-index: 2;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 20px;
  padding: 26px 4.5vw;
  border-bottom: 1px solid var(--cp-color-border-secondary);
}
.brand {
  display: flex;
  align-items: center;
  gap: 12px;
  font-size: 16px;
  font-weight: 650;
  letter-spacing: -0.4px;
}
.brand-mark {
  width: 36px;
  height: 36px;
}
.home-header nav {
  display: flex;
  align-items: center;
  gap: 24px;
}
.console-link {
  display: inline-flex;
  align-items: center;
  gap: 12px;
  font-size: 13px;
}
.console-link:hover {
  color: var(--cosmic-accent);
}
.universe {
  position: relative;
  flex: 1;
  min-height: 620px;
  height: calc(100dvh - 158px);
}
.title-block {
  position: absolute;
  top: 42px;
  left: 4.5vw;
  z-index: 1;
  pointer-events: none;
}
.eyebrow {
  display: flex;
  align-items: center;
  gap: 9px;
  font-family: 'JetBrains Mono Variable', monospace;
  font-size: 10px;
  letter-spacing: 2.5px;
  color: var(--cosmic-accent);
}
.eyebrow span {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: currentColor;
}
h1 {
  margin-top: 16px;
  font-size: clamp(36px, 4vw, 54px);
  font-weight: 450;
  line-height: 1.12;
  letter-spacing: -2px;
}
h1 span {
  display: block;
  margin-top: 12px;
  font-size: 15px;
  font-weight: 400;
  letter-spacing: 3px;
}
.intro {
  margin-top: 18px;
  color: var(--cp-color-text-secondary);
  font-size: 12px;
  letter-spacing: 0.4px;
}
.scene-area {
  position: absolute;
  inset: 0;
}
.scene-caption {
  position: absolute;
  bottom: 24px;
  left: 4.5vw;
  display: flex;
  align-items: center;
  gap: 10px;
  font-family: 'JetBrains Mono Variable', monospace;
  font-size: 10px;
  color: var(--cp-color-text-secondary);
  pointer-events: none;
}
.status-dot {
  height: 5px;
  width: 5px;
  border-radius: 50%;
  background: var(--cosmic-accent);
}
.caption-divider {
  opacity: 0.4;
  margin: 0 4px;
}
.scene-toolbar {
  position: absolute;
  right: 4.5vw;
  bottom: 16px;
  display: flex;
  align-items: center;
  gap: 6px;
}
.gesture-hint {
  margin-right: 12px;
  color: var(--cp-color-text-tertiary);
  font-size: 11px;
}
.scene-toolbar button {
  display: grid;
  place-items: center;
  width: 36px;
  height: 36px;
  border-radius: 50%;
  background: var(--cp-color-bg-container);
  border: 1px solid var(--cp-color-border-secondary);
  cursor: pointer;
}
.scene-toolbar button:hover {
  color: var(--cosmic-accent);
  border-color: currentColor;
}
.scene-toolbar button:disabled {
  opacity: 0.45;
  cursor: default;
}
.home-footer {
  z-index: 2;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 24px;
  padding: 22px 4.5vw;
  border-top: 1px solid var(--cp-color-border-secondary);
  color: var(--cp-color-text-secondary);
  font-size: 11px;
}
.legend {
  display: flex;
  align-items: center;
  gap: 10px;
}
.flow-swatch {
  width: 22px;
  height: 1px;
  background: var(--cosmic-accent);
}
.local-dot,
.attractor-dot {
  margin-left: 6px;
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: var(--cosmic-local);
}
.attractor-dot {
  background: var(--cosmic-accent);
}
.sources {
  position: relative;
}
.sources summary {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  list-style: none;
}
.sources summary::-webkit-details-marker {
  display: none;
}
.source-panel {
  position: absolute;
  bottom: 32px;
  left: 50%;
  transform: translateX(-50%);
  width: min(360px, 88vw);
  padding: 20px;
  border: 1px solid var(--cp-color-border-secondary);
  border-radius: 12px;
  background: var(--cp-color-bg-elevated);
  box-shadow: 0 12px 40px #00000012;
  line-height: 1.8;
}
.source-panel strong {
  color: var(--cp-color-text);
  font-weight: 600;
}
.source-panel p {
  margin: 10px 0;
}
.source-panel a {
  display: block;
  margin-top: 6px;
  color: var(--cosmic-accent);
}
.footer-coordinate {
  font-family: 'JetBrains Mono Variable', monospace;
  font-size: 10px;
  letter-spacing: 1px;
}
@media (max-width: 700px) {
  .home-header {
    padding: 20px;
  }
  .brand {
    font-size: 12px;
    gap: 8px;
  }
  .brand-mark {
    width: 30px;
    height: 30px;
  }
  .home-header nav {
    gap: 12px;
  }
  .console-link {
    font-size: 12px;
    gap: 6px;
  }
  .universe {
    min-height: 620px;
  }
  .title-block {
    top: 32px;
    left: 24px;
  }
  .scene-area {
    top: 100px;
    bottom: 72px;
  }
  .scene-caption {
    left: 24px;
    bottom: 62px;
    font-size: 9px;
    gap: 6px;
  }
  .scene-toolbar {
    right: 24px;
    bottom: 12px;
  }
  .home-footer {
    position: relative;
    flex-wrap: wrap;
    padding: 20px 24px;
    gap: 12px;
  }
  .footer-coordinate {
    display: none;
  }
  .sources {
    position: static;
  }
  .source-panel {
    bottom: calc(100% + 8px);
    left: 24px;
    right: 24px;
    width: auto;
    transform: none;
  }
}
@media (max-width: 380px) {
  .brand span {
    display: none;
  }
}
</style>
