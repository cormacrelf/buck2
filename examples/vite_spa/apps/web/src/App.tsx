import { lazy, Suspense } from 'react';
import { BrowserRouter, Route, Routes } from 'react-router';
import { Layout } from '@/components/Layout';
import { RouteFallback } from '@/components/RouteFallback';
import { TerrainFallback } from '@/components/TerrainFallback';
import { env } from '@/env';
import { flags } from '@/featureFlags';

// One chunk per page. The home page is lazy too, so the entry chunk holds only
// the shell, the router and the i18n bootstrap.
const HomePage = lazy(() => import('@/pages/HomePage'));
const NotesPage = lazy(() => import('@/pages/NotesPage'));
const NotePage = lazy(() => import('@/pages/NotePage'));
const GalleryPage = lazy(() => import('@/pages/GalleryPage'));
const TracksPage = lazy(() => import('@/pages/TracksPage'));
const TerrainPage = lazy(() => import('@/pages/TerrainPage'));
const SpeciesPage = lazy(() => import('@/pages/SpeciesPage'));
const AboutPage = lazy(() => import('@/pages/AboutPage'));
const NotFoundPage = lazy(() => import('@/pages/NotFoundPage'));

export function App() {
  return (
    <BrowserRouter basename={env.baseUrl}>
      <Suspense fallback={<RouteFallback />}>
        <Routes>
          <Route element={<Layout />}>
            <Route index element={<HomePage />} />
            <Route path="notes" element={<NotesPage />} />
            <Route path="notes/:slug" element={<NotePage />} />
            <Route path="gallery" element={<GalleryPage />} />
            <Route path="tracks" element={<TracksPage />} />
            {/* A boundary of its own, so that the wait for this page shows while it lasts. It can
                be long under buck2: the page needs a slow build step and npm packages that no
                other page loads. */}
            <Route
              path="terrain"
              element={
                <Suspense fallback={<TerrainFallback />}>
                  <TerrainPage />
                </Suspense>
              }
            />
            {flags.speciesChecklist && <Route path="species" element={<SpeciesPage />} />}
            <Route path="about" element={<AboutPage />} />
            <Route path="*" element={<NotFoundPage />} />
          </Route>
        </Routes>
      </Suspense>
    </BrowserRouter>
  );
}
