import { lazy, Suspense } from 'react';
import { BrowserRouter, Route, Routes } from 'react-router';
import { Layout } from '@/components/Layout';
import { RouteFallback } from '@/components/RouteFallback';
import { env } from '@/env';
import { flags } from '@/featureFlags';

// One chunk per page. The home page is lazy too, so the entry chunk holds only
// the shell, the router and the i18n bootstrap.
const HomePage = lazy(() => import('@/pages/HomePage'));
const NotesPage = lazy(() => import('@/pages/NotesPage'));
const NotePage = lazy(() => import('@/pages/NotePage'));
const GalleryPage = lazy(() => import('@/pages/GalleryPage'));
const TracksPage = lazy(() => import('@/pages/TracksPage'));
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
            {flags.speciesChecklist && <Route path="species" element={<SpeciesPage />} />}
            <Route path="about" element={<AboutPage />} />
            <Route path="*" element={<NotFoundPage />} />
          </Route>
        </Routes>
      </Suspense>
    </BrowserRouter>
  );
}
