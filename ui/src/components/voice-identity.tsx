"use client";

import { useCallback, useEffect, useState } from "react";
import { Loader2, Mic, CheckCircle2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { type MemoryGateway, type VoiceProfile } from "@/lib/memory";

export function VoiceIdentity({ gateway }: { gateway: MemoryGateway }) {
  const [profile, setProfile] = useState<VoiceProfile | null>(null);
  const [loading, setLoading] = useState(true);

  const [enrolling, setEnrolling] = useState(false);
  const [samples, setSamples] = useState<number[][]>([]);
  const [recording, setRecording] = useState(false);
  // Kept apart from `loading`, which is only the first read. Reusing that flag
  // for the save meant a failed save left the component showing the initial
  // spinner for ever — no error, no way back, because the branch that renders
  // `error` had already been switched off.
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const targetSamples = 3;
  const collected = samples.length >= targetSamples;

  useEffect(() => {
    gateway.getVoiceProfile().then((p) => {
      setProfile(p);
      setLoading(false);
    }).catch(() => {
      setLoading(false);
    });
  }, [gateway]);

  const startRecording = useCallback(async () => {
    setError(null);
    try {
      await gateway.startDictation();
      setRecording(true);
    } catch (e) {
      setError(String(e));
    }
  }, [gateway]);

  /// Enrolment only ends when the profile is actually on disk. Tearing down
  /// the enrolling view first — as this did — threw away three recordings on
  /// any failure and hid the reason.
  const commit = useCallback(
    async (collectedSamples: number[][]) => {
      setError(null);
      setSaving(true);
      try {
        const p = await gateway.saveVoiceProfile(collectedSamples);
        setProfile(p);
        setEnrolling(false);
        setSamples([]);
      } catch (e) {
        setError(String(e));
      } finally {
        setSaving(false);
      }
    },
    [gateway],
  );

  const stopRecording = useCallback(async () => {
    try {
      const embedding = await gateway.stopAndMeasureVoice();
      setRecording(false);

      const newSamples = [...samples, embedding];
      setSamples(newSamples);

      if (newSamples.length >= targetSamples) {
        await commit(newSamples);
      }
    } catch (e) {
      setRecording(false);
      setError(String(e));
    }
  }, [gateway, samples, targetSamples, commit]);

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-[12.5px] text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" />
        Carregando identidade...
      </div>
    );
  }

  if (enrolling) {
    return (
      <div className="space-y-3 rounded-lg border border-sky-500/20 bg-sky-500/5 p-4">
        <div className="flex items-center justify-between">
          <h4 className="text-[13px] font-medium text-sky-700 dark:text-sky-300">
            {collected
              ? `${samples.length} amostras gravadas`
              : `Gravando amostra ${samples.length + 1} de ${targetSamples}`}
          </h4>
          <span className="text-[11px] text-muted-foreground">
            {collected ? "Salvando o perfil" : "Leia a frase em voz alta"}
          </span>
        </div>

        {!collected && (
          <p className="text-[14px] italic text-foreground/80">
            &ldquo;O EchoBrain guarda as minhas memórias de forma segura.&rdquo;
          </p>
        )}

        <div className="flex items-center gap-3 pt-2">
          {collected ? (
            <Button
              size="sm"
              variant="outline"
              className="h-8 gap-2"
              disabled={saving}
              onClick={() => commit(samples)}
            >
              {saving && <Loader2 className="size-3 animate-spin" />}
              {saving ? "Salvando..." : "Tentar novamente"}
            </Button>
          ) : (
            <Button
              size="sm"
              variant={recording ? "default" : "outline"}
              className={`h-8 gap-2 ${recording ? "bg-red-500 hover:bg-red-500/90" : ""}`}
              onMouseDown={startRecording}
              onMouseUp={stopRecording}
              onMouseLeave={() => { if (recording) stopRecording(); }}
            >
              <Mic className={`size-3 ${recording ? "animate-pulse" : ""}`} />
              {recording ? "Solte para parar" : "Segure e fale"}
            </Button>
          )}

          <Button
            size="sm"
            variant="ghost"
            className="h-8 text-[11px]"
            disabled={saving || recording}
            onClick={() => { setEnrolling(false); setSamples([]); setError(null); }}
          >
            Cancelar
          </Button>
        </div>

        {error && <p className="text-[11px] text-red-500">{error}</p>}
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {profile ? (
        <div className="flex items-center justify-between rounded-lg border border-green-500/20 bg-green-500/5 p-3">
          <div className="space-y-1">
            <div className="flex items-center gap-1.5 text-[12.5px] font-medium text-green-700 dark:text-green-300">
              <CheckCircle2 className="size-3.5" />
              Identidade de voz configurada
            </div>
            <p className="text-[11px] text-muted-foreground">
              {profile.enrolledFrom} amostras cadastradas. O EchoBrain filtrará vozes desconhecidas.
            </p>
          </div>
          <Button
            size="sm"
            variant="outline"
            className="h-7 text-[11px]"
            onClick={() => { setSamples([]); setEnrolling(true); }}
          >
            Refazer
          </Button>
        </div>
      ) : (
        <div className="rounded-lg border border-border/50 bg-background/50 p-3">
          <p className="text-[12px] leading-relaxed text-muted-foreground mb-3">
            O EchoBrain não sabe como é a sua voz. Ele ouvirá qualquer pessoa que falar perto do computador.
            Cadastre sua voz para que ele ignore ruídos e outras pessoas.
          </p>
          <Button
            size="sm"
            variant="outline"
            className="h-8 text-[12px]"
            onClick={() => { setSamples([]); setEnrolling(true); }}
          >
            <Mic className="size-3 mr-1.5" />
            Cadastrar minha voz
          </Button>
        </div>
      )}
    </div>
  );
}
