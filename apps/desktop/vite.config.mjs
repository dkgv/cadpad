import {defineConfig} from 'vite';
import {frontendNotices} from '../../scripts/frontend_notices.mjs';

export default defineConfig({plugins:[frontendNotices()]});
