#ifndef ANIME_FCITX5_ANIME_CONFIG_H
#define ANIME_FCITX5_ANIME_CONFIG_H

#include "ipc_protocol.h"
#include "session_state.h"

#include <fcitx-config/configuration.h>
#include <fcitx-config/enum.h>
#include <fcitx-config/option.h>

#include <algorithm>
#include <cstddef>

namespace fcitx {

FCITX_CONFIG_ENUM(AnimeCandidateLayoutMode, Auto, Horizontal, Vertical);

FCITX_CONFIGURATION(
    AnimeConfig,
    Option<bool, NoConstrain<bool>, DefaultMarshaller<bool>, ToolTipAnnotation> smartSpacing{
        this,
        "SmartSpacing",
        "Auto trailing space after commit (with punctuation swallow)",
        true,
        NoConstrain<bool>(),
        DefaultMarshaller<bool>(),
        ToolTipAnnotation(
            "Automatically append a space after selecting a word/sentence, and delete it "
            "if followed by closing punctuation (. , ? ! ; : ) ] \")."),
    };
    Option<bool, NoConstrain<bool>, DefaultMarshaller<bool>, ToolTipAnnotation> spaceCommits{
        this,
        "SpaceCommits",
        "Space commits top candidate (disable for multi-word sentence input)",
        false,
        NoConstrain<bool>(),
        DefaultMarshaller<bool>(),
        ToolTipAnnotation(
            "When unchecked, Space stays inside preedit so you can compose full phrases "
            "or sentences before selecting."),
    };
    Option<AnimeCandidateLayoutMode,
           NoConstrain<AnimeCandidateLayoutMode>,
           DefaultMarshaller<AnimeCandidateLayoutMode>,
           ToolTipAnnotation>
        candidateLayout{
            this,
            "CandidateLayout",
            "Candidate list layout (Auto / Horizontal / Vertical)",
            AnimeCandidateLayoutMode::Auto,
            NoConstrain<AnimeCandidateLayoutMode>(),
            DefaultMarshaller<AnimeCandidateLayoutMode>(),
            ToolTipAnnotation(
                "Auto keeps single words compact/horizontal and automatically switches to "
                "vertical paging for multi-word sentences or wide candidates."),
        };
    Option<int, IntConstrain> maxCandidates{
        this,
        "MaxCandidates",
        "Maximum candidates per query (3-9)",
        9,
        IntConstrain(3, 9),
    };
    Option<int, IntConstrain> longCandidatePageSize{
        this,
        "LongCandidatePageSize",
        "Candidates per page for multi-word/long results (3-9)",
        5,
        IntConstrain(3, 9),
    };
    Option<bool> showCandidateComments{
        this,
        "ShowCandidateComments",
        "Show dictionary status & correction labels (✓ / typo / phonetic / ? raw)",
        true,
    };
    Option<bool> enablePhonetic{
        this,
        "EnablePhonetic",
        "Enable phonetic spelling correction",
        true,
    };
    Option<bool> enableNeural{
        this,
        "EnableNeural",
        "Enable async neural reranking and continuation",
        true,
    };);

}  // namespace fcitx

namespace anime {

inline ipc::EngineConfig toEngineConfig(const fcitx::AnimeConfig &cfg) {
    ipc::EngineConfig out;
    out.smart_spacing = *cfg.smartSpacing;
    out.space_commits = *cfg.spaceCommits;
    out.max_candidates =
        static_cast<size_t>(std::clamp(*cfg.maxCandidates, 3, 9));
    out.enable_phonetic = *cfg.enablePhonetic;
    out.enable_neural = *cfg.enableNeural;
    return out;
}

inline CandidatePresentationOptions toPresentationOptions(const fcitx::AnimeConfig &cfg) {
    CandidatePresentationOptions out;
    switch (*cfg.candidateLayout) {
        case fcitx::AnimeCandidateLayoutMode::Auto:
            out.layout = LayoutPreference::Auto;
            break;
        case fcitx::AnimeCandidateLayoutMode::Horizontal:
            out.layout = LayoutPreference::Horizontal;
            break;
        case fcitx::AnimeCandidateLayoutMode::Vertical:
            out.layout = LayoutPreference::Vertical;
            break;
    }
    out.max_candidates = static_cast<size_t>(std::clamp(*cfg.maxCandidates, 3, 9));
    out.long_page_size = static_cast<size_t>(std::clamp(*cfg.longCandidatePageSize, 3, 9));
    out.show_comments = *cfg.showCandidateComments;
    return out;
}

}  // namespace anime

#endif  // ANIME_FCITX5_ANIME_CONFIG_H
