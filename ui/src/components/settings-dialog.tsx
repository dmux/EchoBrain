"use client";

import { useCallback, useEffect, useState } from "react";
import { KeyRound, Loader2, Settings2, TriangleAlert, Volume2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Slider } from "@/components/ui/slider";
import {
  type Connection,
  type MemoryGateway,
  MemoryGatewayError,
  type Voice,
  type VoiceSettings,
} from "@/lib/memory";
import { VoiceIdentity } from "@/components/voice-identity";

/**
 * Bedrock inference profiles worth offering, cheapest first.
 *
 * Not a catalogue — the account may list two dozen, most of them retired or not
 * entitled. These are the ones that actually answered when probed, cheapest
 * first, and the first is the recommendation: answering five memories in three
 * sentences does not reward a larger model, and every spoken question pays for
 * whichever one is set.
 *
 * A model that is listed but not entitled returns `ResourceNotFoundException`
 * until the Anthropic use-case form is submitted in the Bedrock console. That
 * failure names the form in its own words — see `BedrockError::Invoke` — rather
 * than sending the reader to check a model id that was never wrong.
 *
 * The text field stays, because the ids are account- and region-specific and
 * this list cannot be right for everyone.
 */
/**
 * "7h 12min", "12min", "agora".
 *
 * Coarse on purpose: a session measured in hours does not need seconds ticking
 * next to it, and a countdown precise to the second reads as something you are
 * supposed to watch.
 */
function humanDuration(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  if (hours > 0) return minutes > 0 ? `${hours}h ${minutes}min` : `${hours}h`;
  if (minutes > 0) return `${minutes}min`;
  return "agora";
}

const SUGGESTED_MODELS = [
  {
    id: "us.anthropic.claude-haiku-4-5-20251001-v1:0",
    label: "Haiku 4.5 · mais barato",
  },
  { id: "us.anthropic.claude-sonnet-4-6", label: "Sonnet 4.6" },
  { id: "us.anthropic.claude-opus-4-5-20251101-v1:0", label: "Opus 4.5" },
] as const;

/**
 * Everything about speaking and answering, in one place.
 *
 * # Two things this screen refuses to hide
 *
 * **The similarity floor is a number.** It could have been "conservador /
 * equilibrado / agressivo", and that would read better. But it is the single
 * dial that decides whether the app is allowed to compose a confident sentence
 * out of memories that have nothing to do with the question, and dressing that
 * up as a mood would hide the one thing worth understanding about it.
 *
 * **The cost is stated before it is incurred.** Answering is the only feature
 * here that bills per use. It is off until a model is named, and the screen
 * says what naming one means.
 */
export function SettingsDialog({ gateway }: { gateway: MemoryGateway }) {
  const [open, setOpen] = useState(false);
  const [settings, setSettings] = useState<VoiceSettings | null>(null);
  const [voices, setVoices] = useState<Voice[]>([]);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);

  // The connection keeps its own status and its own Save. It is a different
  // command that fails in different ways — an unreachable account, a profile
  // that does not exist — and folding those into the voice footer would report
  // a credentials problem next to the speech-rate slider.
  const [connection, setConnection] = useState<Connection | null>(null);
  const [connectionSaving, setConnectionSaving] = useState(false);
  const [connectionStatus, setConnectionStatus] = useState<string | null>(null);
  const [ssoLoggingIn, setSsoLoggingIn] = useState(false);

  // The countdown is derived from `sso.expiresAt` rather than stored, so it
  // stays honest while the dialog sits open — otherwise "expira em 2min" would
  // still say that ten minutes later, which is worse than saying nothing.
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    if (!open) return;
    const timer = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 30_000);
    return () => clearInterval(timer);
  }, [open]);

  const load = useCallback(async () => {
    // Both failures are reported rather than swallowed, and for two different
    // dead ends.
    //
    // `availableVoices` returning empty does not merely hide the dropdown — it
    // disables "Testar voz" through `voices.length === 0`, so a silent catch
    // leaves a button that will not respond and says nothing about why.
    //
    // `getSettings` was worse: it had no catch at all and `load` is called as
    // `void load()`, so a rejection left `settings` at `null` for ever. The
    // dialog then renders its "Carregando…" spinner instead of the form, the
    // test button never exists to be clicked, and nothing anywhere records it.
    try {
      const [current, installed, wiring] = await Promise.all([
        gateway.getSettings(),
        gateway.availableVoices().catch((raw) => {
          setStatus(
            `Não consegui listar as vozes: ${raw instanceof Error ? raw.message : String(raw)}`,
          );
          return [];
        }),
        // Caught rather than awaited into the failure below: the connection
        // section is the one place to *fix* a broken account, so it must still
        // render when reading it is what failed.
        gateway.getConnection().catch((raw) => {
          setConnectionStatus(
            `Não consegui ler a conexão: ${raw instanceof Error ? raw.message : String(raw)}`,
          );
          return null;
        }),
      ]);
      setSettings(current);
      setVoices(installed);
      setConnection(wiring);
    } catch (raw) {
      setStatus(
        `Não consegui carregar as configurações: ${raw instanceof Error ? raw.message : String(raw)}`,
      );
    }
  }, [gateway]);

  /**
   * Load when the dialog opens, not in an effect on `open`.
   *
   * Opening is an event, and the panel's own `settled` comment explains why
   * this codebase keeps setState out of effect bodies. Reloading each time also
   * means the screen reflects a `config.toml` edited by hand between openings.
   */
  const onOpenChange = useCallback(
    (next: boolean) => {
      setOpen(next);
      setStatus(null);
      setConnectionStatus(null);
      if (next) void load();
    },
    [load],
  );

  const patchConnection = useCallback((change: Partial<Connection>) => {
    setConnection((current) => (current ? { ...current, ...change } : current));
    setConnectionStatus(null);
  }, []);

  async function saveConnection() {
    if (!connection) return;
    setConnectionSaving(true);
    try {
      // Only the editable half crosses: `profiles` and `envOverrides` are facts
      // about the machine, and sending them back would let this screen claim an
      // override had gone away when it had not.
      const applied = await gateway.setConnection({
        endpoint: connection.endpoint,
        region: connection.region,
        profile: connection.profile,
        githubLogin: connection.githubLogin,
      });
      setConnection(applied);
      setConnectionStatus("Conexão trocada. Vale a partir da próxima busca.");
    } catch (raw) {
      setConnectionStatus(raw instanceof Error ? raw.message : String(raw));
    } finally {
      setConnectionSaving(false);
    }
  }

  async function loginSso() {
    if (!connection?.profile) return;
    setSsoLoggingIn(true);
    setConnectionStatus(null);
    try {
      await gateway.loginSso(connection.profile);
      // Read the connection back rather than assuming: the new expiry — and
      // whether this login produced a renewable token at all — is written by
      // the CLI into a file only the backend can see.
      setConnection(await gateway.getConnection());
      setNow(Math.floor(Date.now() / 1000));
      setConnectionStatus("Sessão SSO renovada.");
    } catch (raw) {
      setConnectionStatus(raw instanceof Error ? raw.message : String(raw));
    } finally {
      setSsoLoggingIn(false);
    }
  }

  const patch = useCallback((change: Partial<VoiceSettings>) => {
    setSettings((current) => (current ? { ...current, ...change } : current));
    setStatus(null);
  }, []);

  async function save() {
    if (!settings) return;
    setSaving(true);
    try {
      const applied = await gateway.setSettings(settings);
      setSettings(applied);
      setStatus("Salvo.");
    } catch (raw) {
      setStatus(raw instanceof Error ? raw.message : String(raw));
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="size-7 text-muted-foreground hover:text-foreground"
          aria-label="Configurações"
        >
          <Settings2 className="size-4" />
        </Button>
      </DialogTrigger>

      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="text-[15px]">Voz e respostas</DialogTitle>
          <DialogDescription className="text-[12.5px]">
            Como o EchoBrain fala com você, e o que ele pode responder.
          </DialogDescription>
        </DialogHeader>

        {!settings ? (
          // The failure is shown here, not only in the footer: the footer lives
          // inside the form below, so with `settings` still null a reported
          // error had nowhere to appear and this spinner span for ever.
          <div className="space-y-3 py-8">
            <div className="flex items-center gap-2 text-[13px] text-muted-foreground">
              {!status && <Loader2 className="size-3.5 animate-spin" aria-hidden />}
              {status ? "Não foi possível abrir as configurações." : "Carregando…"}
            </div>
            {status && (
              <>
                <p className="text-[12px] text-red-500" role="alert">
                  {status}
                </p>
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8 text-[12px]"
                  onClick={() => {
                    setStatus(null);
                    void load();
                  }}
                >
                  Tentar novamente
                </Button>
              </>
            )}
          </div>
        ) : (
          <div className="space-y-6 py-1">
            {connection && (
              <section className="space-y-3">
                <h3 className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground/70">
                  Conexão
                </h3>

                <div className="space-y-1.5">
                  <Label htmlFor="profile" className="text-[12.5px]">
                    Perfil AWS
                  </Label>
                  {/*
                    A menu when this machine has profiles, a text field when it
                    does not. The empty list is a real state — a machine configured
                    purely through environment variables has no `~/.aws/config` —
                    and an empty menu would read as "this app is broken" rather
                    than "nothing is configured yet".
                  */}
                  {connection.profiles.length === 0 ? (
                    <Input
                      id="profile"
                      value={connection.profile ?? ""}
                      onChange={(event) =>
                        patchConnection({ profile: event.target.value || null })
                      }
                      placeholder="vazio = cadeia de credenciais padrão"
                      className="h-9 font-mono text-[12px]"
                    />
                  ) : (
                    <select
                      id="profile"
                      value={connection.profile ?? ""}
                      onChange={(event) =>
                        patchConnection({ profile: event.target.value || null })
                      }
                      className="h-9 w-full rounded-lg border border-border/60 bg-background/50 px-2.5 text-[13px] outline-none focus-visible:border-ring"
                    >
                      <option value="">Cadeia de credenciais padrão</option>
                      {connection.profiles.map((name) => (
                        <option key={name} value={name}>
                          {name}
                        </option>
                      ))}
                    </select>
                  )}
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      className="h-7 text-[11.5px]"
                      disabled={!connection.profile || ssoLoggingIn}
                      onClick={() => void loginSso()}
                    >
                      {ssoLoggingIn ? (
                        <Loader2 className="mr-1.5 size-3 animate-spin" aria-hidden />
                      ) : (
                        <KeyRound className="mr-1.5 size-3" aria-hidden />
                      )}
                      {ssoLoggingIn ? "Autenticando…" : "Entrar via SSO"}
                    </Button>
                    <span className="text-[11px] text-muted-foreground">
                      abre o navegador
                    </span>
                  </div>

                  {/*
                    Nothing at all when there is no SSO session: a profile with
                    static keys, or one nobody has logged into yet. Neither is a
                    problem, and a permanent empty slot reading "sem sessão"
                    would be noise on every machine that never uses SSO.
                  */}
                  {connection.sso &&
                    (() => {
                      const left = connection.sso.expiresAt - now;
                      const expired = left <= 0;

                      // The renewable case is the quiet one, and deliberately
                      // so: the SDK renews the token from inside this process,
                      // five minutes before it lapses, with no browser. There
                      // is nothing for the user to do, so it gets one muted
                      // line rather than a box.
                      if (connection.sso.renewable && !expired) {
                        return (
                          <p className="text-[11.5px] leading-relaxed text-muted-foreground">
                            Sessão <code className="rounded bg-muted/60 px-1 font-mono text-[10.5px]">{connection.sso.session}</code>{" "}
                            válida por mais {humanDuration(left)} e{" "}
                            <strong>renovada sozinha</strong> enquanto o app
                            estiver aberto — até o limite de sessão configurado
                            no IAM Identity Center.
                          </p>
                        );
                      }

                      // Everything else is amber, because everything else ends
                      // in a search that fails: either the token is already
                      // dead, or it will die on a clock nobody is watching and
                      // nothing will renew it.
                      return (
                        <p
                          className="flex gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 p-2.5 text-[11.5px] leading-relaxed text-amber-700 dark:text-amber-300"
                          role="status"
                        >
                          <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
                          <span>
                            {expired ? (
                              <>Sessão SSO expirada — entre novamente para voltar a buscar.</>
                            ) : (
                              <>Sessão expira em {humanDuration(left)}.</>
                            )}{" "}
                            {!connection.sso.renewable && (
                              <>
                                Este perfil <strong>não renova sozinho</strong>:
                                está no formato antigo (
                                <code className="rounded bg-muted/60 px-1 font-mono text-[10.5px]">
                                  sso_start_url
                                </code>{" "}
                                direto no perfil), e o token gravado não tem
                                refresh. Para renovar sem navegador, mova o
                                perfil para um bloco{" "}
                                <code className="rounded bg-muted/60 px-1 font-mono text-[10.5px]">
                                  [sso-session]
                                </code>{" "}
                                com{" "}
                                <code className="rounded bg-muted/60 px-1 font-mono text-[10.5px]">
                                  sso_registration_scopes = sso:account:access
                                </code>{" "}
                                e entre uma vez.
                              </>
                            )}
                          </span>
                        </p>
                      );
                    })()}
                </div>

                <div className="space-y-1.5">
                  <Label htmlFor="region" className="text-[12.5px]">
                    Região
                  </Label>
                  <Input
                    id="region"
                    value={connection.region}
                    onChange={(event) =>
                      patchConnection({ region: event.target.value })
                    }
                    placeholder="us-east-1"
                    className="h-9 font-mono text-[12px]"
                  />
                  <p className="text-[11.5px] leading-relaxed text-muted-foreground">
                    A região da <strong>API</strong>, não a do seu perfil — é ela
                    que o SigV4 assina. Um perfil em <code className="rounded bg-muted/60 px-1 font-mono text-[10.5px]">sa-east-1</code>{" "}
                    assinando para uma API em <code className="rounded bg-muted/60 px-1 font-mono text-[10.5px]">us-east-1</code>{" "}
                    falha como se fosse problema de credencial, e não é.
                  </p>
                </div>

                <div className="space-y-1.5">
                  <Label htmlFor="endpoint" className="text-[12.5px]">
                    Endpoint da API
                  </Label>
                  <Input
                    id="endpoint"
                    value={connection.endpoint}
                    onChange={(event) =>
                      patchConnection({ endpoint: event.target.value })
                    }
                    placeholder="https://….execute-api.us-east-1.amazonaws.com/"
                    className="h-9 font-mono text-[12px]"
                  />
                </div>

                <div className="space-y-1.5">
                  <Label htmlFor="github" className="text-[12.5px]">
                    Login do GitHub
                  </Label>
                  <Input
                    id="github"
                    value={connection.githubLogin ?? ""}
                    onChange={(event) =>
                      patchConnection({ githubLogin: event.target.value || null })
                    }
                    placeholder="opcional"
                    className="h-9 font-mono text-[12px]"
                  />
                  <p className="text-[11.5px] leading-relaxed text-muted-foreground">
                    Só atribuição, gravada junto da memória. Nunca autorização —
                    quem pode ler o quê vem da assinatura, não daqui.
                  </p>
                </div>

                {connection.envOverrides.length > 0 && (
                  // The one thing this screen cannot fix from inside itself. The
                  // value saved here is live now, but `Config::load` lets these
                  // variables win at startup, so the next launch would quietly
                  // come back on the old account with nothing to explain it.
                  <p
                    className="flex gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 p-2.5 text-[11.5px] leading-relaxed text-amber-700 dark:text-amber-300"
                    role="status"
                  >
                    <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
                    <span>
                      {connection.envOverrides.join(", ")}{" "}
                      {connection.envOverrides.length === 1
                        ? "está definida"
                        : "estão definidas"}{" "}
                      no ambiente e tem prioridade sobre o arquivo{" "}
                      <em>na próxima abertura</em>. O que você salvar aqui vale
                      agora, mas volta atrás quando o app reiniciar — abra pelo
                      Finder, ou limpe a variável.
                    </span>
                  </p>
                )}

                <div className="flex items-center justify-between gap-3">
                  <span
                    className="text-[12px] text-muted-foreground"
                    role={
                      connectionStatus && !connectionStatus.startsWith("Conexão") && !connectionStatus.startsWith("Sessão")
                        ? "alert"
                        : undefined
                    }
                  >
                    {connectionStatus}
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-8"
                    disabled={connectionSaving}
                    onClick={() => void saveConnection()}
                  >
                    {connectionSaving ? "Trocando…" : "Trocar conexão"}
                  </Button>
                </div>
              </section>
            )}

            <section className="space-y-3">
              <h3 className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground/70">
                Voz
              </h3>

              <div className="space-y-1.5">
                <Label htmlFor="voice" className="text-[12.5px]">
                  Voz do sistema
                </Label>
                {voices.length === 0 ? (
                  <p className="text-[12px] text-muted-foreground">
                    Nenhuma voz em português instalada. Ajustes do Sistema →
                    Acessibilidade → Conteúdo falado.
                  </p>
                ) : (
                  <select
                    id="voice"
                    value={settings.ttsVoice ?? ""}
                    onChange={(event) => {
                      const selectedVoice = voices.find(v => v.name === event.target.value);
                      patch({ 
                        ttsVoice: event.target.value || null,
                        ttsEngine: selectedVoice ? selectedVoice.engine : "system"
                      })
                    }}
                    className="h-9 w-full rounded-lg border border-border/60 bg-background/50 px-2.5 text-[13px] outline-none focus-visible:border-ring"
                  >
                    <option value="">Automática</option>
                    <optgroup label="Sistema (Offline)">
                      {voices.filter(v => v.engine === 'system').map((voice) => (
                        <option key={voice.name} value={voice.name}>
                          {voice.name}
                        </option>
                      ))}
                    </optgroup>
                    <optgroup label="Amazon Polly (Nuvem)">
                      {voices.filter(v => v.engine === 'polly').map((voice) => (
                        <option key={voice.name} value={voice.name}>
                          {voice.name}
                        </option>
                      ))}
                    </optgroup>
                  </select>
                )}
              </div>

              <div className="space-y-1.5">
                <div className="flex items-baseline justify-between">
                  <Label htmlFor="rate" className="text-[12.5px]">
                    Velocidade
                  </Label>
                  <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
                    {settings.ttsRate} ppm
                  </span>
                </div>
                <Slider
                  id="rate"
                  min={120}
                  max={280}
                  step={5}
                  value={[settings.ttsRate]}
                  onValueChange={([value]) => patch({ ttsRate: value })}
                />
              </div>

              <Button
                variant="outline"
                size="sm"
                className="h-8 text-[12px]"
                disabled={testing || voices.length === 0}
                onClick={async () => {
                  setTesting(true);
                  try {
                    // Saved first: the voice being tested is the one that will
                    // be used, not the one that was configured last launch.
                    await gateway.setSettings(settings);
                    await gateway.speak(
                      "Assim que eu vou ler as suas memórias.",
                    );
                  } catch (error) {
                    // The message, not a generic stand-in. This is the only
                    // place the reason surfaces — the audio simply not playing
                    // looks identical for a rejected voice id, an expired SSO
                    // session and a settings file that could not be written.
                    setStatus(
                      error instanceof MemoryGatewayError
                        ? error.message
                        : `Não consegui falar com essa voz: ${String(error)}`,
                    );
                  } finally {
                    setTesting(false);
                  }
                }}
              >
                {testing ? (
                  <Loader2 className="size-3 animate-spin" aria-hidden />
                ) : (
                  <Volume2 className="size-3" aria-hidden />
                )}
                Testar voz
              </Button>
            </section>

            <section className="space-y-3 border-t border-border/50 pt-5">
              <h3 className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground/70">
                Identidade de Voz
              </h3>
              <VoiceIdentity gateway={gateway} />

              <label className="flex items-start gap-2.5 rounded-lg border border-border/50 bg-background/50 p-3">
                <input
                  type="checkbox"
                  className="mt-0.5 size-3.5 accent-foreground"
                  checked={settings.unknownVoice === "ignore"}
                  onChange={(event) =>
                    patch({
                      unknownVoice: event.target.checked ? "ignore" : "allow",
                    })
                  }
                />
                <span className="space-y-1">
                  <span className="block text-[12.5px]">
                    Ignorar vozes que não são a minha
                  </span>
                  <span className="block text-[11px] leading-relaxed text-muted-foreground">
                    No modo conversa, descarta o que não parecer com a voz
                    cadastrada — a televisão, quem está na sala, e a própria fala
                    do EchoBrain. O atalho ⌥Espaço nunca é filtrado: apertar uma
                    tecla é uma ordem, e ela é obedecida mesmo com a voz
                    diferente por um resfriado.
                  </span>
                </span>
              </label>
            </section>

            <section className="space-y-3 border-t border-border/50 pt-5">
              <h3 className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground/70">
                Respostas
              </h3>

              <div className="space-y-1.5">
                <Label htmlFor="model" className="text-[12.5px]">
                  Modelo no Bedrock
                </Label>
                <Input
                  id="model"
                  value={settings.answerModel ?? ""}
                  onChange={(event) =>
                    patch({ answerModel: event.target.value || null })
                  }
                  placeholder="vazio = não responder, só buscar"
                  className="h-9 font-mono text-[12px]"
                />
                <div className="flex flex-wrap items-center gap-1.5">
                  {SUGGESTED_MODELS.map((model) => (
                    <button
                      key={model.id}
                      type="button"
                      onClick={() => patch({ answerModel: model.id })}
                      aria-pressed={settings.answerModel === model.id}
                      className={
                        settings.answerModel === model.id
                          ? "rounded-full border border-sky-500/30 bg-sky-500/15 px-2 py-0.5 text-[11px] text-sky-600 dark:text-sky-300"
                          : "rounded-full border border-border/60 px-2 py-0.5 text-[11px] text-muted-foreground transition-colors hover:text-foreground"
                      }
                    >
                      {model.label}
                    </button>
                  ))}
                </div>
                <p className="text-[11.5px] leading-relaxed text-muted-foreground">
                  Vazio, o app nunca gasta token: captura e busca continuam
                  funcionando. Com um modelo, cada pergunta falada custa uma
                  chamada — e ela dispara sozinha quando o microfone fecha, sem
                  confirmação. A tarefa aqui é estreita (ler cinco memórias e
                  responder em três frases), então o modelo pequeno faz tão bem
                  quanto o grande. Se a sua conta tiver outros perfis, liste com{" "}
                  <code className="rounded bg-muted/60 px-1 font-mono text-[10.5px]">
                    aws bedrock list-inference-profiles
                  </code>
                  . Vale a partir da próxima pergunta, sem reiniciar.
                </p>
              </div>

              <div className="space-y-1.5">
                <div className="flex items-baseline justify-between">
                  <Label htmlFor="floor" className="text-[12.5px]">
                    Semelhança mínima
                  </Label>
                  <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
                    {Math.round(settings.minSimilarity * 100)}%
                  </span>
                </div>
                <Slider
                  id="floor"
                  min={0}
                  max={90}
                  step={5}
                  value={[Math.round(settings.minSimilarity * 100)]}
                  onValueChange={([value]) =>
                    patch({ minSimilarity: value / 100 })
                  }
                />
                <p className="text-[11.5px] leading-relaxed text-muted-foreground">
                  Abaixo disso o app responde{" "}
                  <em>não encontrei nada</em> em vez de entregar as memórias ao
                  modelo. Baixo demais e ele escreve uma resposta convincente a
                  partir de lembranças que não têm nada a ver com a pergunta —
                  este é o número que impede isso.
                </p>
              </div>
            </section>

            <div className="flex items-center justify-between gap-3 border-t border-border/50 pt-4">
              <span
                className="text-[12px] text-muted-foreground"
                role={status && status !== "Salvo." ? "alert" : undefined}
              >
                {status}
              </span>
              <Button
                size="sm"
                className="h-8"
                disabled={saving}
                onClick={() => void save()}
              >
                {saving ? "Salvando…" : "Salvar"}
              </Button>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
