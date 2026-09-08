import {
  RefreshRounded,
  SettingsRounded,
  VpnLockRounded,
} from '@mui/icons-material'
import {
  Alert,
  Box,
  Button,
  Divider,
  IconButton,
  Stack,
  Switch,
  TextField,
  Typography,
} from '@mui/material'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { BaseDialog } from '@/components/base'
import { useVerge } from '@/hooks/use-verge'
import {
  discoverOpenConnect,
  getOpenConnectSettings,
  getOpenConnectStatus,
  installOpenConnect,
  saveOpenConnectSettings,
  setOpenConnectConnected,
} from '@/services/cmds'
import { showNotice } from '@/services/notice-service'
import type { TranslationKey } from '@/types/generated/i18n-keys'

const EMPTY_SETTINGS: IOpenConnectSettings = {
  name: 'CUHK(SZ) VPN',
  executable: '',
  endpoint: 'https://vpn.cuhk.edu.cn',
  protocol: 'anyconnect',
  authGroup: 'CUHK(SZ)',
  username: '',
  vpnInterface: 'SV-Connection',
  vpncScript: '',
  physicalInterface: '',
  routePrefixes: ['10.0.0.0/8', '137.189.0.0/16'],
  directDomains: ['cuhksz.edu.cn', 'cuhk.edu.cn', 'cuhk.edu.hk'],
  dnsServers: ['10.20.232.75'],
}

const parseList = (value: string) =>
  value
    .split(/[\n,]/)
    .map((item) => item.trim())
    .filter(Boolean)

export const OpenConnectControl = () => {
  const { t } = useTranslation()
  const { verge, patchVerge } = useVerge()
  const [settings, setSettings] = useState(EMPTY_SETTINGS)
  const [password, setPassword] = useState('')
  const [status, setStatus] = useState<IOpenConnectStatus>({
    configured: false,
    hasPassword: false,
    connected: false,
  })
  const [dialogOpen, setDialogOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [discovery, setDiscovery] = useState<IOpenConnectDiscovery>()

  const refresh = async () => {
    const [storedSettings, currentStatus, detected] = await Promise.all([
      getOpenConnectSettings(),
      getOpenConnectStatus(),
      discoverOpenConnect(),
    ])
    setSettings((current) => {
      const next = storedSettings ?? current
      return {
        ...next,
        executable: detected.executable ?? next.executable,
        vpncScript: detected.vpncScript ?? next.vpncScript,
      }
    })
    setStatus(currentStatus)
    setDiscovery(detected)
  }

  useEffect(() => {
    void refresh().catch(showNotice.error)
    const timer = window.setInterval(() => {
      void getOpenConnectStatus()
        .then(setStatus)
        .catch(() => {})
    }, 3000)
    return () => window.clearInterval(timer)
  }, [])

  const save = async () => {
    setBusy(true)
    try {
      await saveOpenConnectSettings(settings, password || undefined)
      setPassword('')
      await refresh()
      setDialogOpen(false)
      showNotice.success('shared.feedback.notifications.common.saveSuccess')
    } catch (error) {
      showNotice.error(error)
    } finally {
      setBusy(false)
    }
  }

  const toggleCombined = async (enabled: boolean) => {
    if (enabled && (!status.configured || !status.hasPassword)) {
      setDialogOpen(true)
      return
    }

    setBusy(true)
    try {
      if (enabled) {
        await setOpenConnectConnected(true)
        try {
          await patchVerge({ enable_tun_mode: true })
        } catch (error) {
          await setOpenConnectConnected(false).catch(() => {})
          throw error
        }
      } else {
        await patchVerge({ enable_tun_mode: false })
        await setOpenConnectConnected(false)
      }
      await refresh()
    } catch (error) {
      showNotice.error(error)
      await refresh().catch(() => {})
    } finally {
      setBusy(false)
    }
  }

  const install = async () => {
    setInstalling(true)
    try {
      const detected = await installOpenConnect()
      setDiscovery(detected)
      setSettings((current) => ({
        ...current,
        executable: detected.executable ?? current.executable,
        vpncScript: detected.vpncScript ?? current.vpncScript,
      }))
      showNotice.success('home.components.openConnect.installSuccess')
    } catch (error) {
      showNotice.error(error)
    } finally {
      setInstalling(false)
    }
  }

  const combined = status.connected && verge?.enable_tun_mode === true

  return (
    <>
      <Divider sx={{ my: 1.5 }} />
      <Stack
        direction="row"
        sx={{ alignItems: 'center', justifyContent: 'space-between' }}
      >
        <Stack direction="row" spacing={1} sx={{ alignItems: 'center' }}>
          <VpnLockRounded color={combined ? 'success' : 'disabled'} />
          <Box>
            <Typography variant="body2" sx={{ fontWeight: 600 }}>
              {t('home.components.openConnect.combined')}
            </Typography>
            <Typography variant="caption" color="text.secondary">
              {status.connected
                ? t('home.components.openConnect.connected', {
                    name: settings.name,
                  })
                : status.configured
                  ? t('home.components.openConnect.ready', {
                      name: settings.name,
                    })
                  : t('home.components.openConnect.notConfigured')}
            </Typography>
          </Box>
        </Stack>
        <Stack direction="row" sx={{ alignItems: 'center' }}>
          <IconButton size="small" onClick={() => setDialogOpen(true)}>
            <SettingsRounded fontSize="small" />
          </IconButton>
          <Switch
            checked={combined}
            disabled={busy}
            onChange={(_, checked) => void toggleCombined(checked)}
          />
        </Stack>
      </Stack>

      <BaseDialog
        open={dialogOpen}
        title={t('home.components.openConnect.settingsTitle')}
        okBtn={t('shared.actions.save')}
        cancelBtn={t('shared.actions.cancel')}
        loading={busy || installing}
        onClose={() => setDialogOpen(false)}
        onCancel={() => setDialogOpen(false)}
        onOk={() => void save()}
        contentSx={{ width: 520 }}
      >
        <Alert severity="info" sx={{ mb: 2 }}>
          {t('home.components.openConnect.passwordHint')}
        </Alert>
        <Alert
          severity={discovery?.executable ? 'success' : 'warning'}
          sx={{ mb: 2 }}
          action={
            <Stack direction="row" spacing={0.5}>
              <Button
                size="small"
                startIcon={<RefreshRounded />}
                disabled={installing}
                onClick={() => void refresh().catch(showNotice.error)}
              >
                {t('home.components.openConnect.rescan')}
              </Button>
              {!discovery?.executable && discovery?.installerAvailable && (
                <Button
                  size="small"
                  disabled={installing}
                  onClick={() => void install()}
                >
                  {t(
                    installing
                      ? 'home.components.openConnect.installing'
                      : 'home.components.openConnect.install',
                  )}
                </Button>
              )}
            </Stack>
          }
        >
          {discovery?.executable
            ? t('home.components.openConnect.detected', {
                path: discovery.executable,
              })
            : discovery?.installerAvailable === false
              ? t('home.components.openConnect.installUnavailable', {
                  platform: discovery.platform,
                })
              : t('home.components.openConnect.notFound')}
        </Alert>
        <Stack spacing={1.5} sx={{ pt: 0.5 }}>
          {(
            [
              ['name', 'name'],
              ['executable', 'executable'],
              ['endpoint', 'endpoint'],
              ['protocol', 'protocol'],
              ['authGroup', 'authGroup'],
              ['username', 'username'],
              ['vpnInterface', 'vpnInterface'],
              ['vpncScript', 'vpncScript'],
              ['physicalInterface', 'physicalInterface'],
            ] as const
          ).map(([field, label]) => (
            <TextField
              key={field}
              size="small"
              label={t(
                `home.components.openConnect.fields.${label}` as TranslationKey,
              )}
              value={settings[field]}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  [field]: event.target.value,
                }))
              }
            />
          ))}
          {(
            [
              ['routePrefixes', 'routePrefixes'],
              ['directDomains', 'directDomains'],
              ['dnsServers', 'dnsServers'],
            ] as const
          ).map(([field, label]) => (
            <TextField
              key={field}
              size="small"
              multiline
              minRows={2}
              label={t(
                `home.components.openConnect.fields.${label}` as TranslationKey,
              )}
              value={settings[field].join('\n')}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  [field]: parseList(event.target.value),
                }))
              }
            />
          ))}
          <TextField
            size="small"
            type="password"
            autoComplete="new-password"
            label={t('home.components.openConnect.fields.password')}
            placeholder={
              status.hasPassword
                ? t('home.components.openConnect.passwordSaved')
                : undefined
            }
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
          <Typography variant="caption" color="text.secondary">
            {t('home.components.openConnect.keepPassword')}
          </Typography>
        </Stack>
      </BaseDialog>
    </>
  )
}
