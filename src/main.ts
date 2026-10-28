import '@fontsource-variable/inter';
import './app.css';
import { mount } from 'svelte';
import { apiPromise } from './lib/api';
import { setLanguage, storedLanguage } from './lib/i18n';
import App from './App.svelte';

// Load the catalog remembered from the last run before the first paint; the
// saved setting is applied again once the settings store has loaded.
Promise.all([apiPromise, setLanguage(storedLanguage())]).then(() => {
  mount(App, { target: document.getElementById('app')! });
});
